//! The `write` tool's core: a batch of operations, checked whole and then
//! committed as one (SPEC §5).

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use schemars::JsonSchema;
use serde::Deserialize;
use ulid::Ulid;

use crate::brain::Brain;
use crate::git::{self, GitError};
use crate::id::{self, EntityId, ItemId};
use crate::index::{Index, IndexError, Staged};
use crate::lock::{LockError, WriteLock};
use crate::recollection::{self, FrontMatter, Kind, Link, LinkType, Recollection, State};

/// One operation in a batch.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// Create a Recollection.
    Create(Create),
    /// Change some fields of a Recollection; `body` and lists are replaced
    /// whole.
    Update(Update),
    /// Replace a Recollection with a new one that records the old ID in
    /// `supersedes`; the old one is deleted.
    Supersede(Supersede),
    /// Delete a Recollection.
    Delete(Delete),
    /// Add a Link from a Recollection.
    Link(LinkChange),
    /// Remove a Link from a Recollection.
    Unlink(LinkChange),
}

/// The fields of a new Recollection; the server fills in the rest.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Create {
    pub kind: Kind,
    /// A short, specific title.
    pub title: String,
    /// One line saying what the Recollection holds.
    pub summary: String,
    /// Markdown. Some Kinds need level-2 sections: decision (Context, Decision,
    /// Why), caveat (Symptom, Cause, Avoidance), how-to (Steps).
    pub body: String,
    /// At least one `about` Link to an Entity; `relates-to` Links go to other
    /// Recollections.
    pub links: Vec<LinkInput>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Version ranges of the Entities this holds for, keyed by Entity ID.
    #[serde(default)]
    pub applies_to: BTreeMap<String, String>,
    /// A name for the new Recollection that later operations in the batch can
    /// use in place of its ID, as `ref:<name>`.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// Any other field, reported as a problem rather than refused outright,
    /// so a rejection lists it with every other problem.
    #[serde(flatten)]
    #[schemars(skip)]
    pub unexpected: BTreeMap<String, serde_json::Value>,
}

/// The fields of a Recollection to change; those left out keep their values.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Update {
    /// The Recollection's ID.
    pub id: String,
    /// The `version` you last saw, from `get`, `search` or `write`; the same
    /// one even if an earlier operation in this batch changes the Recollection.
    pub version: String,
    pub kind: Option<Kind>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub body: Option<String>,
    pub links: Option<Vec<LinkInput>>,
    pub tags: Option<Vec<String>>,
    pub applies_to: Option<BTreeMap<String, String>>,
    /// Any other field, reported as a problem rather than refused outright,
    /// so a rejection lists it with every other problem.
    #[serde(flatten)]
    #[schemars(skip)]
    pub unexpected: BTreeMap<String, serde_json::Value>,
}

/// A Recollection to replace, and its replacement.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Supersede {
    /// The ID of the Recollection to replace.
    pub id: String,
    /// The `version` you last saw, from `get`, `search` or `write`; the same
    /// one even if an earlier operation in this batch changes the Recollection.
    pub version: String,
    /// The new Recollection, with the same fields as `create`.
    pub replacement: Create,
    /// Any other field, reported as a problem rather than refused outright,
    /// so a rejection lists it with every other problem.
    #[serde(flatten)]
    #[schemars(skip)]
    pub unexpected: BTreeMap<String, serde_json::Value>,
}

/// A Recollection to delete.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Delete {
    pub id: String,
    /// The `version` you last saw, from `get`, `search` or `write`; the same
    /// one even if an earlier operation in this batch changes the Recollection.
    pub version: String,
    /// Any other field, reported as a problem rather than refused outright,
    /// so a rejection lists it with every other problem.
    #[serde(flatten)]
    #[schemars(skip)]
    pub unexpected: BTreeMap<String, serde_json::Value>,
}

/// A Link to add or remove.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct LinkChange {
    /// The ID of the Recollection the Link starts from.
    pub from: String,
    #[serde(rename = "type")]
    pub link_type: LinkType,
    /// The ID of the Entity or Recollection the Link points at, or
    /// `ref:<name>` for one an earlier operation in the batch creates.
    pub to: String,
    /// The `version` you last saw of the Recollection the Link starts from;
    /// the same one even if an earlier operation in this batch changes it.
    pub from_version: String,
    /// The version a `depends-on` Link carries.
    pub version: Option<String>,
    /// Any other field, reported as a problem rather than refused outright,
    /// so a rejection lists it with every other problem.
    #[serde(flatten)]
    #[schemars(skip)]
    pub unexpected: BTreeMap<String, serde_json::Value>,
}

/// A Link as an Agent declares it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkInput {
    #[serde(rename = "type")]
    pub link_type: LinkType,
    /// The ID of the Entity or Recollection the Link points at, or
    /// `ref:<name>` for one an earlier operation in the batch creates.
    pub to: String,
}

/// The most characters a `body` may hold from `write` (SPEC §5.2). It keeps
/// one idea per Recollection; a hand-edited file over it is still read.
const BODY_LIMIT: usize = 8_000;

/// The fields of a Recollection the server fills in, which an Agent never
/// sends (SPEC §5.1).
const SERVER_FILLED: [&str; 7] = [
    "id",
    "created",
    "updated",
    "state",
    "abandoned_at",
    "branch",
    "supersedes",
];

/// The check a problem failed (SPEC §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Schema,
    Concurrency,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Check::Schema => "schema",
            Check::Concurrency => "concurrency",
        })
    }
}

/// One reason a batch was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// The operation's index in the batch; `None` for the batch as a whole.
    pub index: Option<usize>,
    pub check: Check,
    pub message: String,
}

/// Why a batch wasn't written.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("the batch failed its checks")]
    Rejected(Vec<Problem>),
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error("couldn't write {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Index(#[from] IndexError),
}

/// A committed batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// One per operation, in order.
    pub outcomes: Vec<Outcome>,
    /// The other Recollections the batch changed so that their Links follow
    /// one it removed (SPEC §5.1).
    pub followed: Vec<Followed>,
    /// Links to a Recollection the batch removed that stay, because the file
    /// they start from no longer parses.
    pub links_left: Vec<LinkLeft>,
    /// The new `version` of every Recollection the batch left in place.
    pub versions: BTreeMap<Ulid, String>,
    /// The commit's SHA.
    pub commit: String,
    /// Whether the index took the batch too. If not, the files are still
    /// written and committed, and the index must be refreshed from them.
    pub indexed: bool,
}

/// A Recollection no operation names that the batch changed so its Links
/// follow one the batch removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Followed {
    pub id: Ulid,
    pub kind: Kind,
    pub title: String,
}

/// A Link from `from` to `to`, which the batch removed, that stays because
/// `from`'s file no longer parses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkLeft {
    pub from: Ulid,
    pub to: Ulid,
}

/// What one operation did, to the Recollection it names: for a supersede,
/// the replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub change: Change,
    pub id: Ulid,
    pub kind: Kind,
    pub title: String,
}

/// What an operation changed, with what only that kind of change has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// With the `ref` the new Recollection was given, if any.
    Created {
        reference: Option<String>,
    },
    Updated,
    /// With the Recollection replaced, and the replacement's `ref`.
    Superseded {
        old: Ulid,
        reference: Option<String>,
    },
    Deleted,
    Linked(Link),
    Unlinked(Link),
}

impl Change {
    /// The operation it came from, as the `Action:` trailer names it.
    pub fn action(&self) -> &'static str {
        match self {
            Change::Created { .. } => "create",
            Change::Updated => "update",
            Change::Superseded { .. } => "supersede",
            Change::Deleted => "delete",
            Change::Linked(_) => "link",
            Change::Unlinked(_) => "unlink",
        }
    }
}

/// Checks `operations` and, if every one passes, writes them as one commit
/// under the write lock, updating `index` to match. `now` is when the batch
/// is written.
pub fn write(
    brain: &Brain,
    index: &mut Index,
    operations: Vec<Operation>,
    lock_timeout: Duration,
    now: SystemTime,
) -> Result<Written, WriteError> {
    let lock = WriteLock::take(brain.root(), lock_timeout)?;
    let mut batch = Batch::new(brain.root(), now);
    if operations.is_empty() {
        batch.problems.push(Problem {
            index: None,
            check: Check::Schema,
            message: "the batch has no operations".into(),
        });
    }
    for (index, operation) in operations.into_iter().enumerate() {
        batch.index = index;
        match operation {
            Operation::Create(create) => batch.create(create)?,
            Operation::Update(update) => batch.update(update)?,
            Operation::Supersede(supersede) => batch.supersede(supersede)?,
            Operation::Delete(delete) => batch.delete(delete)?,
            Operation::Link(change) => batch.link(change)?,
            Operation::Unlink(change) => batch.unlink(change)?,
        }
    }
    if !batch.problems.is_empty() {
        return Err(WriteError::Rejected(batch.problems));
    }
    batch.follow_links(index)?;
    let message = message(&batch.outcomes, &batch.followed);
    let mut changes = Vec::new();
    let mut versions = BTreeMap::new();
    for (id, staged) in batch.staged {
        let after = staged.map(|recollection| recollection.render());
        if let Some(text) = &after {
            versions.insert(id, recollection::version(text));
        }
        let before = batch.disk.remove(&id).flatten();
        if before != after {
            changes.push(FileChange {
                path: id::recollection_file(id),
                before,
                after,
            });
        }
    }
    let (commit, indexed) = commit(brain.root(), index, &lock, &changes, &message)?;
    Ok(Written {
        outcomes: batch.outcomes,
        followed: batch.followed,
        links_left: batch.links_left,
        versions,
        commit,
        indexed,
    })
}

/// A batch being checked: the brain's Recollections as its operations so far
/// have left them, and every problem found.
struct Batch<'a> {
    root: &'a Path,
    now: SystemTime,
    /// `now` as front-matter writes it.
    timestamp: String,
    ids: ulid::Generator,
    /// The operation being checked.
    index: usize,
    /// The text on disk of each Recollection read, or `None` if it has no file.
    disk: BTreeMap<Ulid, Option<String>>,
    /// Each Recollection the batch changes: its new content, or `None` if the
    /// batch deletes it.
    staged: BTreeMap<Ulid, Option<Recollection>>,
    /// The ID each `ref` in the batch so far names.
    refs: BTreeMap<String, Ulid>,
    /// Each Recollection the batch removes, with the one that replaces it, if
    /// any.
    removed: BTreeMap<Ulid, Option<Ulid>>,
    outcomes: Vec<Outcome>,
    followed: Vec<Followed>,
    links_left: Vec<LinkLeft>,
    problems: Vec<Problem>,
}

impl<'a> Batch<'a> {
    fn new(root: &'a Path, now: SystemTime) -> Batch<'a> {
        Batch {
            root,
            now,
            timestamp: humantime::format_rfc3339_seconds(now).to_string(),
            ids: ulid::Generator::new(),
            index: 0,
            disk: BTreeMap::new(),
            staged: BTreeMap::new(),
            refs: BTreeMap::new(),
            removed: BTreeMap::new(),
            outcomes: Vec::new(),
            followed: Vec::new(),
            links_left: Vec::new(),
            problems: Vec::new(),
        }
    }

    fn problem(&mut self, check: Check, message: impl Into<String>) {
        self.problems.push(Problem {
            index: Some(self.index),
            check,
            message: message.into(),
        });
    }

    fn schema(&mut self, message: impl Into<String>) {
        self.problem(Check::Schema, message);
    }

    fn create(&mut self, create: Create) -> Result<(), WriteError> {
        self.check_unexpected("create", &create.unexpected, &SERVER_FILLED);
        let reference = create.reference.clone();
        let (id, recollection) = self.new_recollection(create, Vec::new());
        self.outcome(Change::Created { reference }, id, &recollection);
        self.staged.insert(id, Some(recollection));
        Ok(())
    }

    fn supersede(&mut self, supersede: Supersede) -> Result<(), WriteError> {
        let before = self.problems.len();
        self.check_unexpected("supersede", &supersede.unexpected, &[]);
        let replacement = &supersede.replacement;
        self.check_unexpected("replacement", &replacement.unexpected, &SERVER_FILLED);
        let target = self.target(&supersede.id, &supersede.version)?;
        if let Some((old, _)) = &target {
            let old = old.to_string();
            let resolved: Vec<_> = replacement
                .links
                .iter()
                .filter_map(|link| self.resolve(&link.to))
                .collect();
            if resolved.contains(&old) {
                self.schema(format!(
                    "the replacement can't Link to {old}, which it supersedes"
                ));
            }
        }
        let supersedes = target.iter().map(|(old, _)| old.to_string()).collect();
        let reference = supersede.replacement.reference.clone();
        let (id, recollection) = self.new_recollection(supersede.replacement, supersedes);
        // The replacement is staged even if the operation has problems, so
        // later operations can still be checked against it.
        self.staged.insert(id, Some(recollection.clone()));
        let Some((old, _)) = target else {
            return Ok(());
        };
        if self.problems.len() == before {
            self.outcome(Change::Superseded { old, reference }, id, &recollection);
            self.staged.insert(old, None);
            self.removed.insert(old, Some(id));
        }
        Ok(())
    }

    fn delete(&mut self, delete: Delete) -> Result<(), WriteError> {
        let before = self.problems.len();
        self.check_unexpected("delete", &delete.unexpected, &[]);
        let target = self.target(&delete.id, &delete.version)?;
        if let Some((id, recollection)) = target
            && self.problems.len() == before
        {
            self.outcome(Change::Deleted, id, &recollection);
            self.staged.insert(id, None);
            self.removed.insert(id, None);
        }
        Ok(())
    }

    fn link(&mut self, change: LinkChange) -> Result<(), WriteError> {
        let before = self.problems.len();
        let Some(target) = self.link_target("link", &change)? else {
            return Ok(());
        };
        let link = self.declared_link(change.link_type, &change.to);
        let Some((id, mut recollection)) = target else {
            return Ok(());
        };
        self.check_not_to_itself(id, std::slice::from_ref(&link));
        if recollection.front_matter.links.contains(&link) {
            self.schema(format!(
                "{id} already has the `{}` Link to {}",
                link.link_type.as_str(),
                link.to
            ));
        }
        if self.problems.len() == before {
            recollection.front_matter.links.push(link.clone());
            self.changed_link(Change::Linked(link), id, recollection);
        }
        Ok(())
    }

    fn unlink(&mut self, change: LinkChange) -> Result<(), WriteError> {
        let before = self.problems.len();
        let Some(target) = self.link_target("unlink", &change)? else {
            return Ok(());
        };
        let link = Link {
            link_type: change.link_type,
            to: self.resolve(&change.to).unwrap_or(change.to),
        };
        let Some((id, mut recollection)) = target else {
            return Ok(());
        };
        let links = &mut recollection.front_matter.links;
        let count = links.len();
        links.retain(|kept| *kept != link);
        if links.len() == count {
            self.schema(format!(
                "{id} has no `{}` Link to {}",
                link.link_type.as_str(),
                link.to
            ));
        } else if !links.iter().any(|kept| kept.link_type == LinkType::About) {
            self.schema(format!(
                "{id} would have no `about` Link left; a Recollection needs at least one"
            ));
        }
        if self.problems.len() == before {
            self.changed_link(Change::Unlinked(link), id, recollection);
        }
        Ok(())
    }

    /// The Recollection a `link` or `unlink` starts from, if any, or `None`
    /// if the Link starts from an Entity, so nothing more is worth checking.
    fn link_target(
        &mut self,
        op: &str,
        change: &LinkChange,
    ) -> Result<Option<Option<(Ulid, Recollection)>>, WriteError> {
        self.check_unexpected(op, &change.unexpected, &[]);
        if change.version.is_some() {
            self.schema(
                "only a `depends-on` Link carries a `version`, and those start from a Repository",
            );
        }
        if let Some(ItemId::Entity(_)) = ItemId::parse(&change.from) {
            self.schema(format!(
                "{} is an Entity; `link` and `unlink` work only from Recollections for now",
                change.from
            ));
            return Ok(None);
        }
        self.target(&change.from, &change.from_version).map(Some)
    }

    /// Reports each field of `op` that isn't one, saying which of them the
    /// server fills in.
    fn check_unexpected(
        &mut self,
        op: &str,
        unexpected: &BTreeMap<String, serde_json::Value>,
        server_filled: &[&str],
    ) {
        for field in unexpected.keys() {
            if server_filled.contains(&field.as_str()) {
                self.schema(format!(
                    "`{field}` is filled in by the server; leave it out"
                ));
            } else {
                self.schema(format!("`{field}` isn't a field of `{op}`"));
            }
        }
    }

    /// Stages a Recollection whose Links an operation changed.
    fn changed_link(&mut self, change: Change, id: Ulid, mut recollection: Recollection) {
        recollection.front_matter.updated = self.timestamp.clone();
        self.outcome(change, id, &recollection);
        self.staged.insert(id, Some(recollection));
    }

    /// Checks the fields of a new Recollection and builds it, with a new ID.
    /// A new Recollection is staged even if it has problems, so later
    /// operations can still be checked against it.
    fn new_recollection(
        &mut self,
        create: Create,
        supersedes: Vec<String>,
    ) -> (Ulid, Recollection) {
        // Monotonic, so a batch's IDs sort in the batch's order.
        let id = self
            .ids
            .generate_from_datetime(self.now)
            .unwrap_or_else(|overflow| overflow.commit_overflow_random());
        self.check_title(&create.title);
        self.check_summary(&create.summary);
        self.check_body(create.kind, &create.body);
        let links = self.check_links(&create.links);
        self.check_applies_to(&create.applies_to);
        if let Some(reference) = &create.reference {
            self.define(reference, id);
        }
        let recollection = Recollection {
            front_matter: FrontMatter {
                id: id.to_string(),
                kind: create.kind,
                title: create.title.trim().to_owned(),
                summary: create.summary.trim().to_owned(),
                state: State::Active,
                abandoned_at: None,
                branch: None,
                applies_to: create.applies_to,
                tags: create.tags,
                links,
                supersedes,
                created: self.timestamp.clone(),
                updated: self.timestamp.clone(),
            },
            body: create.body,
        };
        (id, recollection)
    }

    fn update(&mut self, update: Update) -> Result<(), WriteError> {
        let before = self.problems.len();
        // An `update` names its Recollection by `id`, which comes first.
        self.check_unexpected("update", &update.unexpected, &SERVER_FILLED[1..]);
        let target = self.target(&update.id, &update.version)?;
        if let Some(title) = &update.title {
            self.check_title(title);
        }
        if let Some(summary) = &update.summary {
            self.check_summary(summary);
        }
        if update.kind.is_some() || update.body.is_some() {
            let current = target.as_ref().map(|(_, current)| current);
            let kind = update.kind.or(current.map(|c| c.front_matter.kind));
            let body = update.body.as_deref().or(current.map(|c| c.body.as_str()));
            if let (Some(kind), Some(body)) = (kind, body) {
                self.check_sections(kind, body);
            }
        }
        if let Some(body) = &update.body {
            self.check_body_size(body);
        }
        let links = update.links.as_ref().map(|links| self.check_links(links));
        if let Some(applies_to) = &update.applies_to {
            self.check_applies_to(applies_to);
        }
        let Some((id, mut recollection)) = target else {
            return Ok(());
        };
        if let Some(links) = &links {
            self.check_not_to_itself(id, links);
        }
        if self.problems.len() > before {
            return Ok(());
        }
        let front_matter = &mut recollection.front_matter;
        if let Some(kind) = update.kind {
            front_matter.kind = kind;
        }
        if let Some(title) = update.title {
            front_matter.title = title.trim().to_owned();
        }
        if let Some(summary) = update.summary {
            front_matter.summary = summary.trim().to_owned();
        }
        if let Some(links) = links {
            front_matter.links = links;
        }
        if let Some(tags) = update.tags {
            front_matter.tags = tags;
        }
        if let Some(applies_to) = update.applies_to {
            front_matter.applies_to = applies_to;
        }
        if let Some(body) = update.body {
            recollection.body = body;
        }
        recollection.front_matter.updated = self.timestamp.clone();
        self.outcome(Change::Updated, id, &recollection);
        self.staged.insert(id, Some(recollection));
        Ok(())
    }

    /// Records what an operation did to Recollection `id`.
    fn outcome(&mut self, change: Change, id: Ulid, recollection: &Recollection) {
        self.outcomes.push(Outcome {
            change,
            id,
            kind: recollection.front_matter.kind,
            title: recollection.front_matter.title.clone(),
        });
    }

    /// The existing Recollection an operation changes, as the batch has left
    /// it so far, if `id` names one and its file still has `version`.
    fn target(
        &mut self,
        id: &str,
        version: &str,
    ) -> Result<Option<(Ulid, Recollection)>, WriteError> {
        if id.starts_with("ref:") {
            self.schema(format!(
                "{id} can only name a Link's `to`; give a new Recollection its Links in its own `create`"
            ));
            return Ok(None);
        }
        let ulid = match ItemId::parse(id) {
            Some(ItemId::Recollection(ulid)) => ulid,
            Some(ItemId::Entity(_)) => {
                self.schema(format!("{id} is an Entity, not a Recollection"));
                return Ok(None);
            }
            None => {
                self.schema(format!("{id} isn't a Recollection ID"));
                return Ok(None);
            }
        };
        let Some(text) = self.read(ulid)? else {
            self.schema(format!("no Recollection has the ID {id}"));
            return Ok(None);
        };
        let current = recollection::version(&text);
        if current != version {
            self.problem(
                Check::Concurrency,
                format!(
                    "conflict: {ulid} has changed since version {version}; its current version is {current}"
                ),
            );
        }
        if let Some(staged) = self.staged.get(&ulid) {
            return Ok(match staged {
                Some(recollection) => Some((ulid, recollection.clone())),
                None => {
                    self.schema(format!(
                        "{ulid} is deleted by an earlier operation in this batch"
                    ));
                    None
                }
            });
        }
        match Recollection::parse(&text) {
            Ok(recollection) => Ok(Some((ulid, recollection))),
            Err(error) => {
                self.schema(format!(
                    "{ulid} can't be changed because its file is broken ({error}); the owner must repair it by hand"
                ));
                Ok(None)
            }
        }
    }

    /// The text on disk of Recollection `id`, read once per batch.
    fn read(&mut self, id: Ulid) -> Result<Option<String>, WriteError> {
        if let Some(text) = self.disk.get(&id) {
            return Ok(text.clone());
        }
        let path = self.root.join(id::recollection_file(id));
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => return Err(WriteError::Io { path, source }),
        };
        self.disk.insert(id, text.clone());
        Ok(text)
    }

    /// Whether Recollection `id` exists once the batch so far is applied.
    fn exists(&self, id: Ulid) -> bool {
        match self.staged.get(&id) {
            Some(staged) => staged.is_some(),
            None => self.root.join(id::recollection_file(id)).is_file(),
        }
    }

    fn check_title(&mut self, title: &str) {
        if title.trim().is_empty() {
            self.schema("`title` is empty");
        }
    }

    fn check_summary(&mut self, summary: &str) {
        if summary.trim().is_empty() {
            self.schema("`summary` is empty");
        } else if summary.trim().contains('\n') {
            self.schema("`summary` must be one line");
        }
    }

    fn check_body(&mut self, kind: Kind, body: &str) {
        self.check_sections(kind, body);
        self.check_body_size(body);
    }

    fn check_body_size(&mut self, body: &str) {
        let size = body.chars().count();
        if size > BODY_LIMIT {
            self.schema(format!(
                "`body` is {size} characters, over the limit of {BODY_LIMIT}; split it into \
                 several Recollections, one idea each, linked with `relates-to`"
            ));
        }
    }

    fn check_sections(&mut self, kind: Kind, body: &str) {
        for section in recollection::missing_sections(kind, body) {
            self.schema(format!(
                "a {} needs a `## {section}` section in its body",
                kind.as_str()
            ));
        }
    }

    /// Checks a Recollection's whole new list of Links, returning them as
    /// they are stored.
    fn check_links(&mut self, links: &[LinkInput]) -> Vec<Link> {
        if !links.iter().any(|link| link.link_type == LinkType::About) {
            self.schema("`links` needs at least one `about` Link to an Entity");
        }
        links
            .iter()
            .map(|link| self.declared_link(link.link_type, &link.to))
            .collect()
    }

    /// Resolves and checks a Link an Agent declared from a Recollection,
    /// returning it as it is stored.
    fn declared_link(&mut self, link_type: LinkType, to: &str) -> Link {
        let to = match self.resolve(to) {
            Some(to) => {
                self.check_link(link_type, &to);
                to
            }
            None => {
                self.schema(format!(
                    "{to} names no `ref` given by an earlier operation in this batch"
                ));
                to.to_owned()
            }
        };
        Link { link_type, to }
    }

    /// The ID `to` names: a `ref:<name>` resolved, a Recollection ID in its
    /// canonical form, or anything else as it is. `None` for an unknown ref.
    fn resolve(&self, to: &str) -> Option<String> {
        if let Some(name) = to.strip_prefix("ref:") {
            return self.refs.get(name).map(Ulid::to_string);
        }
        Some(match ItemId::parse(to) {
            Some(ItemId::Recollection(id)) => id.to_string(),
            _ => to.to_owned(),
        })
    }

    /// Gives the new Recollection `id` the name `reference` for the rest of
    /// the batch.
    fn define(&mut self, reference: &str, id: Ulid) {
        if reference.trim().is_empty() {
            self.schema("`ref` is empty");
        } else if self.refs.contains_key(reference) {
            self.schema(format!(
                "`ref` {reference} is already given by an earlier operation in this batch"
            ));
        } else {
            self.refs.insert(reference.to_owned(), id);
        }
    }

    /// Makes every `relates-to` Link to a Recollection the batch removes
    /// follow it: to its replacement, or out of the file (SPEC §5.1). The
    /// Links are found in the Recollections the batch writes and, through
    /// `index`, in every other.
    fn follow_links(&mut self, index: &Index) -> Result<(), WriteError> {
        if self.removed.is_empty() {
            return Ok(());
        }
        let mut sources: Vec<Ulid> = self.staged.keys().copied().collect();
        for removed in self.removed.keys() {
            for source in index.linking_to(&removed.to_string(), LinkType::RelatesTo)? {
                if let Some(ItemId::Recollection(source)) = ItemId::parse(&source)
                    && !sources.contains(&source)
                {
                    sources.push(source);
                }
            }
        }
        for source in sources {
            let (mut recollection, staged) = match self.staged.get(&source) {
                Some(Some(recollection)) => (recollection.clone(), true),
                Some(None) => continue,
                None => {
                    // The index may be behind a hand edit that removed the file.
                    let Some(text) = self.read(source)? else {
                        continue;
                    };
                    match Recollection::parse(&text) {
                        Ok(recollection) => (recollection, false),
                        Err(_) => {
                            self.leave_links(source, &text);
                            continue;
                        }
                    }
                }
            };
            let links = &recollection.front_matter.links;
            let followed = self.followed_links(source, links);
            if followed == *links {
                continue;
            }
            recollection.front_matter.links = followed;
            if !staged {
                recollection.front_matter.updated = self.timestamp.clone();
                self.followed.push(Followed {
                    id: source,
                    kind: recollection.front_matter.kind,
                    title: recollection.front_matter.title.clone(),
                });
            }
            self.staged.insert(source, Some(recollection));
        }
        Ok(())
    }

    /// `links`, the Links of Recollection `source`, with each to a removed
    /// Recollection repointed at its replacement or dropped, and none
    /// repeated or to `source` itself.
    fn followed_links(&self, source: Ulid, links: &[Link]) -> Vec<Link> {
        let mut followed: Vec<Link> = Vec::new();
        for link in links {
            let mut link = link.clone();
            if link.link_type == LinkType::RelatesTo
                && let Some(ItemId::Recollection(to)) = ItemId::parse(&link.to)
                && let Some(&replacement) = self.removed.get(&to)
            {
                // A replacement is never removed in its own batch, since only
                // a `ref` names it there and removing takes an ID.
                match replacement {
                    Some(replacement) if replacement != source => link.to = replacement.to_string(),
                    _ => continue,
                }
            }
            if !followed.contains(&link) {
                followed.push(link);
            }
        }
        followed
    }

    /// Records each `relates-to` Link to a removed Recollection that stays in
    /// `source`, whose file `text` no longer parses.
    fn leave_links(&mut self, source: Ulid, text: &str) {
        for &to in self.removed.keys() {
            if text.contains(&to.to_string()) {
                self.links_left.push(LinkLeft { from: source, to });
            }
        }
    }

    fn check_not_to_itself(&mut self, id: Ulid, links: &[Link]) {
        if links.iter().any(|link| link.to == id.to_string()) {
            self.schema(format!("{id} can't Link to itself"));
        }
    }

    /// Checks one Link from a Recollection.
    fn check_link(&mut self, link_type: LinkType, to: &str) {
        let problem = match link_type {
            LinkType::About if !entity_exists(self.root, to) => Some(format!(
                "the `about` Link points at {to}, which isn't a known Entity"
            )),
            LinkType::RelatesTo => match ItemId::parse(to) {
                Some(ItemId::Recollection(id)) if self.exists(id) => None,
                _ => Some(format!(
                    "the `relates-to` Link points at {to}, which isn't a known Recollection"
                )),
            },
            LinkType::DependsOn | LinkType::Calls => Some(format!(
                "a Recollection can't have a `{}` Link; those go from a Repository",
                link_type.as_str()
            )),
            LinkType::About => None,
        };
        if let Some(problem) = problem {
            self.schema(problem);
        }
    }

    fn check_applies_to(&mut self, applies_to: &BTreeMap<String, String>) {
        for entity in applies_to.keys() {
            if !entity_exists(self.root, entity) {
                self.schema(format!(
                    "`applies_to` names {entity}, which isn't a known Entity"
                ));
            }
        }
    }
}

fn entity_exists(root: &Path, id: &str) -> bool {
    EntityId::parse(id).is_some_and(|id| root.join(id.file()).is_file())
}

/// The commit message for a batch: a generated subject, then trailers (SPEC §5.3).
fn message(outcomes: &[Outcome], followed: &[Followed]) -> String {
    let mut ids: Vec<Ulid> = Vec::new();
    let mut actions: Vec<&str> = Vec::new();
    for outcome in outcomes {
        let old = match outcome.change {
            Change::Superseded { old, .. } => Some(old),
            _ => None,
        };
        for id in old.into_iter().chain([outcome.id]) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if !actions.contains(&outcome.change.action()) {
            actions.push(outcome.change.action());
        }
    }
    let subject = match (outcomes, ids.len(), actions.as_slice()) {
        ([one], _, [action]) => {
            format!("{} {}: {}", verb(action), one.kind.as_str(), one.title)
        }
        (_, count, [action]) => format!("{} {count} Recollections", verb(action)),
        (_, count, _) => format!("Change {count} Recollections"),
    };
    let mut message = format!("{subject}\n\n");
    // Recollections changed only to follow Links are part of the batch too
    // (SPEC §5.1), though the subject names what the operations did.
    for followed in followed {
        if !ids.contains(&followed.id) {
            ids.push(followed.id);
        }
    }
    for id in &ids {
        message.push_str(&format!("Recollection: {id}\n"));
    }
    message.push_str(&format!("Action: {}\nAuthor: agent\n", actions.join(", ")));
    message
}

/// An action as the verb that starts a commit subject.
fn verb(action: &str) -> String {
    let mut chars = action.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

/// One file a batch changes, relative to the brain folder: its text before
/// and after, where `None` means no file.
struct FileChange {
    path: PathBuf,
    before: Option<String>,
    after: Option<String>,
}

impl FileChange {
    fn path_str(&self) -> &str {
        self.path.to_str().expect("brain paths are UTF-8")
    }
}

/// Applies `changes`, to the files and then the index, and commits exactly
/// them, returning the commit's SHA and whether the index took them. If the
/// commit fails, the files and index are put back as they were so nothing is
/// left half-written.
fn commit(
    root: &Path,
    index: &mut Index,
    lock: &WriteLock,
    changes: &[FileChange],
    message: &str,
) -> Result<(String, bool), WriteError> {
    let paths: Vec<&str> = changes.iter().map(FileChange::path_str).collect();
    let result = apply(root, index, lock, changes, message);
    if result.is_err() {
        for change in changes {
            let _ = put(root, &change.path, change.before.as_deref());
        }
        let mut reset = vec!["reset", "--quiet", "--"];
        reset.extend(paths);
        let _ = git::run(root, &reset);
    }
    // The batch has landed, so a failure from here on must not undo it; an
    // index left behind is caught up from the files' hashes.
    let indexed = result?.commit().is_ok();
    let sha = git::run(root, &["rev-parse", "HEAD"])?.trim().to_owned();
    Ok((sha, indexed))
}

/// The part of `commit` that `commit` undoes if it fails.
fn apply<'a>(
    root: &Path,
    index: &'a mut Index,
    lock: &WriteLock,
    changes: &[FileChange],
    message: &str,
) -> Result<Staged<'a>, WriteError> {
    for change in changes {
        put(root, &change.path, change.after.as_deref())?;
    }
    let staged = index.stage(
        lock,
        changes
            .iter()
            .map(|change| (change.path_str(), change.after.as_deref())),
    )?;
    let paths: Vec<&str> = changes.iter().map(FileChange::path_str).collect();
    // `add` stages a deleted path's removal too.
    let mut add = vec!["add", "--"];
    add.extend(&paths);
    git::run(root, &add)?;
    // Naming the paths commits only them, never a hand edit that happens to be
    // staged.
    let mut commit = vec!["commit", "--quiet", "--message", message, "--"];
    commit.extend(&paths);
    git::run(root, &commit)?;
    Ok(staged)
}

/// Writes `text` to `path` under `root`, or removes the file if it's `None`.
fn put(root: &Path, path: &Path, text: Option<&str>) -> Result<(), WriteError> {
    let full = root.join(path);
    let io = |source| WriteError::Io {
        path: full.clone(),
        source,
    };
    match text {
        Some(text) => {
            if let Some(dir) = full.parent() {
                std::fs::create_dir_all(dir).map_err(io)?;
            }
            std::fs::write(&full, text).map_err(io)
        }
        None => match std::fs::remove_file(&full) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io(error)),
            _ => Ok(()),
        },
    }
}
