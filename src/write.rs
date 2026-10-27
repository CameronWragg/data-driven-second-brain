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
use crate::lock::{LockError, WriteLock};
use crate::recollection::{self, FrontMatter, Kind, Link, LinkType, Recollection, State};

/// One operation in a batch.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// Create a Recollection.
    Create(Create),
}

/// The fields of a new Recollection; the server fills in the rest.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
}

/// A Link as an Agent declares it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkInput {
    #[serde(rename = "type")]
    pub link_type: LinkType,
    /// The ID of the Entity or Recollection the Link points at.
    pub to: String,
}

/// The check a problem failed (SPEC §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Schema,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Check::Schema => "schema",
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
}

/// A committed batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub created: Vec<Created>,
    /// The commit's SHA.
    pub commit: String,
}

/// A Recollection the batch created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub id: Ulid,
    pub kind: Kind,
    pub title: String,
    pub version: String,
}

/// Checks `operations` and, if every one passes, writes them as one commit
/// under the write lock. `now` is when the batch is written.
pub fn write(
    brain: &Brain,
    operations: Vec<Operation>,
    lock_timeout: Duration,
    now: SystemTime,
) -> Result<Written, WriteError> {
    let _lock = WriteLock::take(brain.root(), lock_timeout)?;
    let problems = check(brain.root(), &operations);
    if !problems.is_empty() {
        return Err(WriteError::Rejected(problems));
    }
    let timestamp = humantime::format_rfc3339_seconds(now).to_string();
    let mut ids = ulid::Generator::new();
    let recollections: Vec<Recollection> = operations
        .into_iter()
        .map(|Operation::Create(create)| {
            // Monotonic, so a batch's IDs sort in the batch's order.
            let id = ids
                .generate_from_datetime(now)
                .unwrap_or_else(|overflow| overflow.commit_overflow_random());
            new_recollection(id, create, &timestamp)
        })
        .collect();
    let mut files = Vec::new();
    let mut created = Vec::new();
    for recollection in &recollections {
        let id: Ulid = recollection
            .front_matter
            .id
            .parse()
            .expect("the server made this ID");
        let text = recollection.render();
        files.push((id::recollection_file(id), text.clone()));
        created.push(Created {
            id,
            kind: recollection.front_matter.kind,
            title: recollection.front_matter.title.clone(),
            version: recollection::version(&text),
        });
    }
    let commit = commit(brain.root(), &files, &message(&created))?;
    Ok(Written { created, commit })
}

/// Every problem with every operation, in order.
fn check(root: &Path, operations: &[Operation]) -> Vec<Problem> {
    if operations.is_empty() {
        return vec![Problem {
            index: None,
            check: Check::Schema,
            message: "the batch has no operations".into(),
        }];
    }
    let mut problems = Vec::new();
    for (index, Operation::Create(create)) in operations.iter().enumerate() {
        let mut problem = |message: String| {
            problems.push(Problem {
                index: Some(index),
                check: Check::Schema,
                message,
            })
        };
        if create.title.trim().is_empty() {
            problem("`title` is empty".into());
        }
        if create.summary.trim().is_empty() {
            problem("`summary` is empty".into());
        } else if create.summary.trim().contains('\n') {
            problem("`summary` must be one line".into());
        }
        for section in recollection::missing_sections(create.kind, &create.body) {
            problem(format!(
                "a {} needs a `## {section}` section in its body",
                create.kind.as_str()
            ));
        }
        if !create
            .links
            .iter()
            .any(|link| link.link_type == LinkType::About)
        {
            problem("`links` needs at least one `about` Link to an Entity".into());
        }
        for link in &create.links {
            if let Some(message) = check_link(root, link) {
                problem(message);
            }
        }
        for entity in create.applies_to.keys() {
            if !entity_exists(root, entity) {
                problem(format!(
                    "`applies_to` names {entity}, which isn't a known Entity"
                ));
            }
        }
    }
    problems
}

/// What's wrong with a Link from a new Recollection, if anything.
fn check_link(root: &Path, link: &LinkInput) -> Option<String> {
    let to = &link.to;
    match link.link_type {
        LinkType::About if !entity_exists(root, to) => Some(format!(
            "the `about` Link points at {to}, which isn't a known Entity"
        )),
        LinkType::RelatesTo => match ItemId::parse(to) {
            Some(id @ ItemId::Recollection(_)) if root.join(id.file()).is_file() => None,
            _ => Some(format!(
                "the `relates-to` Link points at {to}, which isn't a known Recollection"
            )),
        },
        LinkType::DependsOn | LinkType::Calls => Some(format!(
            "a Recollection can't have a `{}` Link; those go from a Repository",
            link.link_type.as_str()
        )),
        LinkType::About => None,
    }
}

fn entity_exists(root: &Path, id: &str) -> bool {
    EntityId::parse(id).is_some_and(|id| root.join(id.file()).is_file())
}

fn new_recollection(id: Ulid, create: Create, timestamp: &str) -> Recollection {
    Recollection {
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
            links: create
                .links
                .into_iter()
                .map(|link| Link {
                    link_type: link.link_type,
                    to: link.to,
                })
                .collect(),
            supersedes: Vec::new(),
            created: timestamp.to_owned(),
            updated: timestamp.to_owned(),
        },
        body: create.body,
    }
}

/// The commit message for a batch: a generated subject, then trailers (SPEC §5.3).
fn message(created: &[Created]) -> String {
    let subject = match created {
        [one] => format!("Create {}: {}", one.kind.as_str(), one.title),
        many => format!("Create {} Recollections", many.len()),
    };
    let mut message = format!("{subject}\n\n");
    for recollection in created {
        message.push_str(&format!("Recollection: {}\n", recollection.id));
    }
    message.push_str("Action: create\nAuthor: agent\n");
    message
}

/// Writes `files` (paths relative to `root`) and commits exactly them. If the
/// commit fails, the files are removed again so nothing is left half-written.
fn commit(root: &Path, files: &[(PathBuf, String)], message: &str) -> Result<String, WriteError> {
    let paths: Vec<&str> = files
        .iter()
        .map(|(path, _)| path.to_str().expect("brain paths are UTF-8"))
        .collect();
    let result = (|| {
        for (path, text) in files {
            let full = root.join(path);
            let io = |source| WriteError::Io {
                path: full.clone(),
                source,
            };
            if let Some(dir) = full.parent() {
                std::fs::create_dir_all(dir).map_err(io)?;
            }
            std::fs::write(&full, text).map_err(io)?;
        }
        let mut add = vec!["add", "--"];
        add.extend(&paths);
        git::run(root, &add)?;
        // Naming the paths commits only them, never a hand edit that happens
        // to be staged.
        let mut commit = vec!["commit", "--quiet", "--message", message, "--"];
        commit.extend(&paths);
        git::run(root, &commit)?;
        Ok(git::run(root, &["rev-parse", "HEAD"])?.trim().to_owned())
    })();
    if result.is_err() {
        for (path, _) in files {
            let _ = std::fs::remove_file(root.join(path));
        }
        let mut reset = vec!["reset", "--quiet", "--"];
        reset.extend(&paths);
        let _ = git::run(root, &reset);
    }
    result
}
