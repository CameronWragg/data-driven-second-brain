//! The index in `.index/` (SPEC §6.1): one SQLite database, derived entirely
//! from the brain's files and rebuilt in place whenever it doesn't match them
//! ([ADR-0001](../docs/adr/0001-files-as-truth-sqlite-as-index.md)).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::brain::Brain;
use crate::entity::Entity;
use crate::id::{EntityId, ItemId, ItemKind};
use crate::identifiers;
use crate::lock::WriteLock;
use crate::recollection::{self, Kind, LinkType, Recollection, State};

/// What a search asks for (SPEC §6.4, keyword part).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// The words to find; any of them matches.
    pub text: String,
    /// Only Recollections of these Kinds; any Kind if empty.
    pub kinds: Vec<Kind>,
    /// Only Recollections with an `about` Link to one of these Entities.
    pub entities: Vec<String>,
    /// Only Recollections with one of these tags.
    pub tags: Vec<String>,
    /// Whether `abandoned` Recollections are found too.
    pub include_abandoned: bool,
    /// The most results to give.
    pub limit: usize,
    /// Only Items in this Repository's scope (SPEC §6.4): the Repository, the
    /// Entities its own Links point at, and Recollections about any of them.
    /// Every Repository's if `None`.
    pub repository: Option<EntityId>,
    /// The Branches whose Recollections are found besides the default
    /// branch's, which always are.
    pub branches: Branches,
}

/// Which Branches a search finds Recollections on besides the default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Branches {
    All,
    Only {
        /// Branch IDs (SPEC §4.1).
        ids: Vec<String>,
        /// Branch names, matched in every Repository.
        names: Vec<String>,
    },
}

/// One search result, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
    pub summary: String,
    pub version: String,
    /// `None` is the Repository's default branch.
    pub branch: Option<String>,
}

/// What a refresh or rebuild re-indexed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Refreshed {
    /// Whether every table was dropped and built again from every file.
    pub rebuilt: bool,
    /// The files indexed because they are new or their content changed,
    /// relative to the brain folder.
    pub indexed: Vec<PathBuf>,
    /// The files no longer in the brain, dropped from the index.
    pub removed: Vec<PathBuf>,
}

/// Why the index couldn't be read or updated.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("the index failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("couldn't read {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// The index schema this binary builds. Any change to `SCHEMA` bumps it: an
/// index built at another version is rebuilt, never migrated.
const SCHEMA_VERSION: u32 = 2;

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
CREATE TABLE files (path TEXT PRIMARY KEY, hash TEXT NOT NULL) STRICT;
CREATE TABLE items (
    row INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    path TEXT NOT NULL UNIQUE REFERENCES files (path),
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    state TEXT NOT NULL,
    branch TEXT
) STRICT;
CREATE TABLE tags (id TEXT NOT NULL, tag TEXT NOT NULL) STRICT;
CREATE INDEX tags_by_id ON tags (id);
CREATE TABLE links (source TEXT NOT NULL, type TEXT NOT NULL, target TEXT NOT NULL) STRICT;
CREATE INDEX links_by_source ON links (source);
CREATE INDEX links_by_target ON links (target);
CREATE VIRTUAL TABLE text USING fts5 (title, body, split);
CREATE TABLE checkouts (path TEXT PRIMARY KEY, repository TEXT NOT NULL) STRICT;
CREATE INDEX checkouts_by_repository ON checkouts (repository);
";

/// The index's database file, relative to the brain folder.
const FILE: &str = ".index/index.sqlite";

/// How long a connection waits for another's write to finish (SPEC §11).
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest an Entity's summary, the first line of its description, may
/// be in a result.
const ENTITY_SUMMARY: usize = 200;

/// An open connection to the index.
#[derive(Debug)]
pub struct Index {
    connection: Connection,
}

impl Index {
    /// Opens the index of `brain`, creating its database file if there is
    /// none. Its tables may not match this binary until a `refresh`.
    pub fn open(brain: &Brain) -> Result<Index, IndexError> {
        let dir = brain.root().join(".index");
        std::fs::create_dir_all(&dir).map_err(|source| IndexError::Io { path: dir, source })?;
        let connection = Connection::open(brain.root().join(FILE))?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(Index { connection })
    }

    /// Brings the index up to date with the files: rebuilds it if it was built
    /// by another schema version or brain format, and otherwise re-indexes
    /// each file whose content changed.
    pub fn refresh(&mut self, brain: &Brain, _lock: &WriteLock) -> Result<Refreshed, IndexError> {
        let transaction = self.write()?;
        let current = built(&transaction)? == Some((SCHEMA_VERSION, brain.format()));
        let refreshed = index_files(&transaction, brain, !current)?;
        transaction.commit()?;
        Ok(refreshed)
    }

    /// Drops every table and builds them again from every file, inside the
    /// same database file, so other connections to it carry on (SPEC §6.1).
    pub fn rebuild(&mut self, brain: &Brain, _lock: &WriteLock) -> Result<Refreshed, IndexError> {
        let transaction = self.write()?;
        let refreshed = index_files(&transaction, brain, true)?;
        transaction.commit()?;
        Ok(refreshed)
    }

    /// Updates the index for `files`, each a path relative to the brain folder
    /// with its new text, or `None` if it was deleted. Other connections see
    /// nothing until the result is committed, and dropping it undoes it.
    pub fn stage<'a, 'b>(
        &'a mut self,
        _lock: &WriteLock,
        files: impl IntoIterator<Item = (&'b str, Option<&'b str>)>,
    ) -> Result<Staged<'a>, IndexError> {
        let transaction = self.write()?;
        for (path, text) in files {
            put(&transaction, path, text)?;
        }
        Ok(Staged(transaction))
    }

    /// The IDs of the Items with a `link_type` Link to `target`.
    pub fn linking_to(&self, target: &str, link_type: LinkType) -> Result<Vec<String>, IndexError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT source FROM links WHERE target = ?1 AND type = ?2 ORDER BY source",
        )?;
        let sources = statement
            .query_map([target, link_type.as_str()], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(sources)
    }

    /// The Items matching `query`, best first.
    pub fn search(&self, query: &Query) -> Result<Vec<Hit>, IndexError> {
        let Some(matching) = match_expression(&query.text) else {
            return Ok(Vec::new());
        };
        let list = |values: Vec<&str>| {
            (!values.is_empty()).then(|| serde_json::to_string(&values).expect("strings serialise"))
        };
        let kinds = list(query.kinds.iter().map(|kind| kind.as_str()).collect());
        let entities = list(query.entities.iter().map(String::as_str).collect());
        let tags = list(query.tags.iter().map(String::as_str).collect());
        let (all_branches, branch_ids, branch_names) = match &query.branches {
            Branches::All => (true, None, None),
            Branches::Only { ids, names } => (
                false,
                list(ids.iter().map(String::as_str).collect()),
                list(names.iter().map(String::as_str).collect()),
            ),
        };
        let mut statement = self.connection.prepare_cached(
            "WITH scope (id) AS (
                 SELECT ?7
                 UNION SELECT target FROM links
                 WHERE source = ?7 AND (target LIKE 'repo:%' OR target LIKE 'dep:%'))
             SELECT items.id, items.kind, items.title, items.summary, files.hash, items.branch
             FROM text
             JOIN items ON items.row = text.rowid
             JOIN files ON files.path = items.path
             WHERE text MATCH ?1
               AND (?2 OR items.state = 'active')
               AND (?3 IS NULL OR items.kind IN (SELECT value FROM json_each(?3)))
               AND (?4 IS NULL OR EXISTS (
                   SELECT 1 FROM links
                   WHERE links.source = items.id AND links.type = 'about'
                     AND links.target IN (SELECT value FROM json_each(?4))))
               AND (?5 IS NULL OR EXISTS (
                   SELECT 1 FROM tags
                   WHERE tags.id = items.id AND tags.tag IN (SELECT value FROM json_each(?5))))
               AND (?7 IS NULL OR items.id IN scope OR EXISTS (
                   SELECT 1 FROM links
                   WHERE links.source = items.id AND links.type = 'about'
                     AND links.target IN scope))
               AND (?8 OR items.branch IS NULL
                    OR items.branch IN (SELECT value FROM json_each(?9))
                    OR substr(items.branch, instr(items.branch, '#') + 1)
                       IN (SELECT value FROM json_each(?10)))
             ORDER BY bm25(text, 3.0, 1.0, 1.0), items.id
             LIMIT ?6",
        )?;
        let hits = statement
            .query_map(
                params![
                    matching,
                    query.include_abandoned,
                    kinds,
                    entities,
                    tags,
                    query.limit as i64,
                    query.repository.as_ref().map(EntityId::to_string),
                    all_branches,
                    branch_ids,
                    branch_names
                ],
                hit,
            )?
            .collect::<Result<_, _>>()?;
        Ok(hits)
    }

    /// The active Recollections with an `about` Link to `entity`, newest
    /// first, at most `limit` of them, and how many there are in all.
    pub fn about(&self, entity: &EntityId, limit: usize) -> Result<(Vec<Hit>, usize), IndexError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT items.id, items.kind, items.title, items.summary, files.hash, items.branch,
                    count(*) OVER ()
             FROM links
             JOIN items ON items.id = links.source
             JOIN files ON files.path = items.path
             WHERE links.target = ?1 AND links.type = 'about' AND items.state = 'active'
             ORDER BY items.id DESC
             LIMIT ?2",
        )?;
        let mut total = 0;
        let hits = statement
            .query_map(params![entity.to_string(), limit as i64], |row| {
                total = row.get::<_, i64>(6)? as usize;
                hit(row)
            })?
            .collect::<Result<_, _>>()?;
        Ok((hits, total))
    }

    /// Records that a Checkout of `repository` is at `path` on this machine
    /// (SPEC §4.3).
    pub fn record_checkout(
        &mut self,
        path: &Path,
        repository: &EntityId,
    ) -> Result<(), IndexError> {
        let transaction = self.write()?;
        transaction.execute(
            "INSERT OR REPLACE INTO checkouts (path, repository) VALUES (?1, ?2)",
            [path.to_string_lossy().into_owned(), repository.to_string()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Where `repository` is checked out on this machine.
    pub fn checkouts(&self, repository: &EntityId) -> Result<Vec<PathBuf>, IndexError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT path FROM checkouts WHERE repository = ?1 ORDER BY path")?;
        let paths = statement
            .query_map([repository.to_string()], |row| row.get::<_, String>(0))?
            .map(|path| path.map(PathBuf::from))
            .collect::<Result<_, _>>()?;
        Ok(paths)
    }

    /// Starts a write, waiting for any other connection's (SPEC §11).
    fn write(&mut self) -> Result<Transaction<'_>, IndexError> {
        Ok(self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?)
    }
}

/// A result from a row whose first six columns are an Item's ID, kind,
/// title, summary, version and branch.
fn hit(row: &rusqlite::Row) -> rusqlite::Result<Hit> {
    let kind: String = row.get(1)?;
    Ok(Hit {
        id: row.get(0)?,
        kind: ItemKind::parse(&kind).expect("the index stores only known kinds"),
        title: row.get(2)?,
        summary: row.get(3)?,
        version: row.get(4)?,
        branch: row.get(5)?,
    })
}

/// Index changes not yet visible to other connections.
#[must_use = "dropping it undoes the changes"]
pub struct Staged<'a>(Transaction<'a>);

impl Staged<'_> {
    /// Makes the changes visible to every connection.
    pub fn commit(self) -> Result<(), IndexError> {
        Ok(self.0.commit()?)
    }
}

/// The schema version and brain format the index was built at, or `None` if
/// it has never been built.
fn built(transaction: &Transaction) -> Result<Option<(u32, u32)>, IndexError> {
    let has_meta: bool = transaction.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'meta')",
        [],
        |row| row.get(0),
    )?;
    if !has_meta {
        return Ok(None);
    }
    let value = |key: &str| -> Result<Option<u32>, IndexError> {
        let value: Option<String> = transaction
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(value.and_then(|value| value.parse().ok()))
    };
    Ok(value("schema_version")?.zip(value("brain_format")?))
}

/// Indexes every file that is new or changed since the index last saw it,
/// and drops those that are gone, after dropping and recreating every table
/// if `rebuild` is set.
fn index_files(
    transaction: &Transaction,
    brain: &Brain,
    rebuild: bool,
) -> Result<Refreshed, IndexError> {
    if rebuild {
        // Checkouts go too: the index stays derived from the files (ADR-0001),
        // and `serve` records each Checkout again whenever it detects it.
        drop_tables(transaction)?;
        transaction.execute_batch(SCHEMA)?;
        transaction.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1), ('brain_format', ?2)",
            params![SCHEMA_VERSION.to_string(), brain.format().to_string()],
        )?;
    }
    let mut known: BTreeMap<String, String> = transaction
        .prepare("SELECT path, hash FROM files")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut refreshed = Refreshed {
        rebuilt: rebuild,
        ..Refreshed::default()
    };
    for path in item_files(brain.root())? {
        let full = brain.root().join(&path);
        let bytes = std::fs::read(&full).map_err(|source| IndexError::Io { path: full, source })?;
        let text = String::from_utf8_lossy(&bytes);
        if known.remove(&path).as_deref() != Some(&recollection::version(&text)) {
            put(transaction, &path, Some(&text))?;
            refreshed.indexed.push(path.into());
        }
    }
    for path in known.into_keys() {
        put(transaction, &path, None)?;
        refreshed.removed.push(path.into());
    }
    Ok(refreshed)
}

/// Drops every table in the database, whatever schema version made it.
fn drop_tables(transaction: &Transaction) -> Result<(), IndexError> {
    // Dropping a table another references would fail its foreign key, so the
    // check waits for the commit, by when every table is rebuilt.
    transaction.pragma_update(None, "defer_foreign_keys", true)?;
    // Virtual tables first, which takes their shadow tables with them.
    for virtual_tables in [true, false] {
        let names: Vec<String> = transaction
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                   AND (sql LIKE 'CREATE VIRTUAL TABLE%') = ?1",
            )?
            .query_map([virtual_tables], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        for name in names {
            transaction.execute(&format!("DROP TABLE \"{}\"", name.replace('"', "\"\"")), [])?;
        }
    }
    Ok(())
}

/// Every file in the brain that an Item lives in, relative to the brain
/// folder, with `/` between its parts.
fn item_files(root: &Path) -> Result<Vec<String>, IndexError> {
    let mut files = Vec::new();
    let mut dirs = vec![root.join("recollections"), root.join("entities")];
    while let Some(dir) = dirs.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // Git doesn't keep empty folders, so any of them may be missing.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(IndexError::Io { path: dir, source }),
        };
        for entry in entries {
            let entry = entry.map_err(|source| IndexError::Io {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if let Some(relative) = path
                .strip_prefix(root)
                .ok()
                .and_then(Path::to_str)
                .map(|relative| relative.replace(std::path::MAIN_SEPARATOR, "/"))
                && ItemId::from_file(&relative).is_some()
            {
                files.push(relative);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Replaces whatever the index holds for the Item file at `path` with
/// `text`, or drops it if `text` is `None`. A file too broken to parse keeps
/// its hash, so it isn't read again until it changes, but can't be found.
fn put(transaction: &Transaction, path: &str, text: Option<&str>) -> Result<(), IndexError> {
    let id = ItemId::from_file(path).expect("only Item files are indexed");
    let id_text = id.to_string();
    transaction.execute(
        "DELETE FROM text WHERE rowid IN (SELECT row FROM items WHERE path = ?1)",
        [path],
    )?;
    transaction.execute("DELETE FROM tags WHERE id = ?1", [&id_text])?;
    transaction.execute("DELETE FROM links WHERE source = ?1", [&id_text])?;
    transaction.execute("DELETE FROM items WHERE path = ?1", [path])?;
    transaction.execute("DELETE FROM files WHERE path = ?1", [path])?;
    let Some(text) = text else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO files (path, hash) VALUES (?1, ?2)",
        [path, &recollection::version(text)],
    )?;
    let Some(indexed) = Indexed::read(&id, text) else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO items (id, path, kind, title, summary, state, branch)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id_text,
            path,
            indexed.kind.as_str(),
            indexed.title,
            indexed.summary,
            indexed.state.as_str(),
            indexed.branch
        ],
    )?;
    let row = transaction.last_insert_rowid();
    for tag in &indexed.tags {
        transaction.execute(
            "INSERT INTO tags (id, tag) VALUES (?1, ?2)",
            [&id_text, tag],
        )?;
    }
    for link in &indexed.links {
        transaction.execute(
            "INSERT INTO links (source, type, target) VALUES (?1, ?2, ?3)",
            [&id_text, link.link_type.as_str(), &link.to],
        )?;
    }
    let split = identifiers::split(&format!("{}\n{}", indexed.title, indexed.text));
    transaction.execute(
        "INSERT INTO text (rowid, title, body, split) VALUES (?1, ?2, ?3, ?4)",
        params![row, indexed.title, indexed.text, split],
    )?;
    Ok(())
}

/// What the index holds about one Item, read from its file.
struct Indexed {
    kind: ItemKind,
    title: String,
    summary: String,
    state: State,
    branch: Option<String>,
    tags: Vec<String>,
    links: Vec<IndexedLink>,
    /// Everything searched besides the title.
    text: String,
}

/// A Link from either kind of Item, without the fields only some carry.
struct IndexedLink {
    link_type: LinkType,
    to: String,
}

impl Indexed {
    /// Reads the file of Item `id`, or `None` if it doesn't parse.
    fn read(id: &ItemId, text: &str) -> Option<Indexed> {
        match id {
            ItemId::Recollection(_) => {
                let Recollection { front_matter, body } = Recollection::parse(text).ok()?;
                let text = format!(
                    "{}\n{}\n{}",
                    front_matter.summary,
                    front_matter.tags.join(" "),
                    body
                );
                Some(Indexed {
                    kind: ItemKind::Recollection(front_matter.kind),
                    title: front_matter.title,
                    summary: front_matter.summary,
                    state: front_matter.state,
                    branch: front_matter.branch,
                    tags: front_matter.tags,
                    links: front_matter
                        .links
                        .into_iter()
                        .map(|link| IndexedLink {
                            link_type: link.link_type,
                            to: link.to,
                        })
                        .collect(),
                    text,
                })
            }
            ItemId::Entity(entity) => {
                let parsed = Entity::parse(entity, text).ok()?;
                let body = parsed.description.as_str();
                let first_line = body.lines().map(str::trim).find(|line| !line.is_empty());
                let mut summary: String = first_line.unwrap_or_default().to_owned();
                if summary.chars().count() > ENTITY_SUMMARY {
                    summary = summary.chars().take(ENTITY_SUMMARY - 1).collect();
                    summary.push('…');
                }
                Some(Indexed {
                    kind: entity.kind(),
                    title: parsed.name().to_owned(),
                    summary,
                    state: State::Active,
                    branch: None,
                    tags: Vec::new(),
                    links: parsed
                        .links()
                        .iter()
                        .map(|link| IndexedLink {
                            link_type: link.link_type,
                            to: link.to.clone(),
                        })
                        .collect(),
                    text: body.to_owned(),
                })
            }
        }
    }
}

/// An FTS5 query matching any word of `text`, each as written and split at
/// identifier boundaries, or `None` if it has no words. Every word is quoted,
/// so nothing in `text` is read as FTS5 syntax.
fn match_expression(text: &str) -> Option<String> {
    let mut words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    words.extend(identifiers::split(text).split(' ').map(str::to_owned));
    words.retain(|word| !word.is_empty());
    words.sort();
    words.dedup();
    (!words.is_empty()).then(|| {
        words
            .iter()
            .map(|word| format!("\"{word}\""))
            .collect::<Vec<_>>()
            .join(" OR ")
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::*;

    /// A brain folder with the dev marker, so a debug build opens it, and no
    /// git repository, which the index doesn't need.
    struct TestBrain {
        dir: tempfile::TempDir,
        brain: Brain,
    }

    impl TestBrain {
        fn new() -> TestBrain {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("brain.toml"), "format = 1\ndev = true\n").unwrap();
            let brain = Brain::open(dir.path()).unwrap();
            TestBrain { dir, brain }
        }

        fn put(&self, path: &str, text: &str) {
            let file = self.dir.path().join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, text).unwrap();
        }

        fn lock(&self) -> WriteLock {
            WriteLock::take(self.dir.path(), Duration::ZERO).unwrap()
        }

        /// The brain's index, brought up to date.
        fn index(&self) -> Index {
            let mut index = Index::open(&self.brain).unwrap();
            index.refresh(&self.brain, &self.lock()).unwrap();
            index
        }
    }

    const CAVEAT_ID: &str = "01JB3K8V9Q0000000000000000";
    const CAVEAT_PATH: &str = "recollections/01JB3K8V9Q0000000000000000.md";

    /// A caveat about Prisma whose body is `body`.
    fn caveat(body: &str) -> String {
        format!(
            "---
id: {CAVEAT_ID}
kind: caveat
title: Prisma migrations fail silently
summary: Nothing applies without a shadow database.
state: active
abandoned_at: null
branch: null
tags:
- migrations
links:
- type: about
  to: dep:npm:prisma
created: 2026-10-23T10:00:00Z
updated: 2026-10-23T10:00:00Z
---

{body}
"
        )
    }

    fn query(text: &str) -> Query {
        Query {
            text: text.into(),
            kinds: Vec::new(),
            entities: Vec::new(),
            tags: Vec::new(),
            include_abandoned: false,
            limit: 10,
            repository: None,
            branches: Branches::All,
        }
    }

    /// A Repository Entity file for `id`, described as a shadow repository,
    /// with a `link_type` Link to each of `links`.
    fn repository(id: &str, link_type: LinkType, links: &[&str]) -> String {
        let id = EntityId::parse(id).unwrap();
        let mut entity = crate::entity::Entity::new(&id, "repository", "A shadow repository.");
        if let crate::entity::Fields::Repository(fields) = &mut entity.fields {
            fields.links = links
                .iter()
                .map(|to| crate::entity::EntityLink {
                    link_type,
                    to: (*to).into(),
                    version: None,
                    by: None,
                })
                .collect();
        }
        entity.render()
    }

    /// A caveat about `shadow` with ID `id`, about `about`, whose `branch` is
    /// the Branch ID `branch`.
    fn caveat_about(test: &TestBrain, id: &str, about: &str, branch: Option<&str>) {
        let branch = branch.map_or("null".to_owned(), |branch| format!("'{branch}'"));
        test.put(
            &format!("recollections/{id}.md"),
            &caveat("The shadow database is missing.")
                .replace(CAVEAT_ID, id)
                .replace("dep:npm:prisma", about)
                .replace("branch: null", &format!("branch: {branch}")),
        );
    }

    #[test]
    fn a_repository_scope_covers_it_the_entities_it_links_to_and_recollections_about_them() {
        let test = TestBrain::new();
        let (api, web) = ("repo:github.com/owner/api", "repo:github.com/owner/web");
        test.put(
            "entities/repositories/github.com/owner/api.md",
            &repository(api, LinkType::DependsOn, &["dep:npm:prisma"]),
        );
        // web calls api, but a Link into api doesn't bring web into its scope.
        test.put(
            "entities/repositories/github.com/owner/web.md",
            &repository(web, LinkType::Calls, &[api]),
        );
        test.put(
            "entities/dependencies/npm/prisma.md",
            "---\nid: dep:npm:prisma\necosystem: npm\nname: prisma\n---\n\nA shadow ORM.\n",
        );
        let (about_api, about_prisma, about_web) = (
            "01JB3K8V9Q0000000000000001",
            "01JB3K8V9Q0000000000000002",
            "01JB3K8V9Q0000000000000003",
        );
        caveat_about(&test, about_api, api, None);
        caveat_about(&test, about_prisma, "dep:npm:prisma", None);
        caveat_about(&test, about_web, web, None);
        let index = test.index();
        let mut scoped = query("shadow");
        scoped.repository = EntityId::parse(api);

        let hits = index.search(&scoped).unwrap();
        let mut found = ids(&hits);
        found.sort();

        assert_eq!(found, [about_api, about_prisma, "dep:npm:prisma", api]);
        assert_eq!(index.search(&query("shadow")).unwrap().len(), 6);
    }

    #[test]
    fn recollections_on_other_branches_are_found_only_when_asked_for() {
        let test = TestBrain::new();
        let (main, api_x, web_x) = (
            "01JB3K8V9Q0000000000000001",
            "01JB3K8V9Q0000000000000002",
            "01JB3K8V9Q0000000000000003",
        );
        caveat_about(&test, main, "dep:npm:prisma", None);
        caveat_about(
            &test,
            api_x,
            "dep:npm:prisma",
            Some("repo:github.com/owner/api#feature/x"),
        );
        caveat_about(
            &test,
            web_x,
            "dep:npm:prisma",
            Some("repo:github.com/owner/web#feature/x"),
        );
        let index = test.index();
        let on = |ids: &[&str], names: &[&str]| {
            let mut query = query("shadow");
            query.branches = Branches::Only {
                ids: ids.iter().map(|id| (*id).to_owned()).collect(),
                names: names.iter().map(|name| (*name).to_owned()).collect(),
            };
            let mut found: Vec<String> = ids_of(&index.search(&query).unwrap());
            found.sort();
            found
        };

        assert_eq!(on(&[], &[]), [main]);
        assert_eq!(on(&["repo:github.com/owner/api#feature/y"], &[]), [main]);
        assert_eq!(
            on(&["repo:github.com/owner/api#feature/x"], &[]),
            [main, api_x]
        );
        assert_eq!(on(&[], &["feature/x"]), [main, api_x, web_x]);
        let mut all = query("shadow");
        all.branches = Branches::All;
        assert_eq!(index.search(&all).unwrap().len(), 3);
    }

    #[test]
    fn checkouts_are_recorded_by_repository() {
        let test = TestBrain::new();
        let mut index = test.index();
        let api = EntityId::parse("repo:github.com/owner/api").unwrap();

        index.record_checkout(Path::new("/work/api"), &api).unwrap();
        index
            .record_checkout(Path::new("/work/api-2"), &api)
            .unwrap();
        index.record_checkout(Path::new("/work/api"), &api).unwrap();

        assert_eq!(
            index.checkouts(&api).unwrap(),
            [PathBuf::from("/work/api"), PathBuf::from("/work/api-2")]
        );
    }

    fn ids_of(hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|hit| hit.id.clone()).collect()
    }

    fn ids(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.id.as_str()).collect()
    }

    #[test]
    fn a_recollection_is_found_by_a_word_in_its_body() {
        let test = TestBrain::new();
        test.put(
            CAVEAT_PATH,
            &caveat("## Symptom\n\nThe shadow database is missing.\n"),
        );

        let hits = test.index().search(&query("missing")).unwrap();

        assert_eq!(
            hits,
            [Hit {
                id: CAVEAT_ID.into(),
                kind: ItemKind::Recollection(Kind::Caveat),
                title: "Prisma migrations fail silently".into(),
                summary: "Nothing applies without a shadow database.".into(),
                version: crate::recollection::version(&caveat(
                    "## Symptom\n\nThe shadow database is missing.\n"
                )),
                branch: None,
            }]
        );
        assert_eq!(
            ids(&test.index().search(&query("absent")).unwrap()),
            [] as [&str; 0]
        );
    }

    #[test]
    fn a_recollection_is_found_by_one_word_of_an_identifier() {
        let test = TestBrain::new();
        test.put(CAVEAT_PATH, &caveat("Call `getUserById` before migrating."));

        let index = test.index();

        assert_eq!(ids(&index.search(&query("user")).unwrap()), [CAVEAT_ID]);
        assert_eq!(
            ids(&index.search(&query("getUserById")).unwrap()),
            [CAVEAT_ID]
        );
    }

    #[test]
    fn query_text_is_never_read_as_fts5_syntax() {
        let test = TestBrain::new();
        test.put(CAVEAT_PATH, &caveat("The shadow database is missing."));

        let index = test.index();

        for text in [
            "\"shadow",
            "shadow*",
            "NEAR(shadow",
            "body:shadow",
            "-shadow",
        ] {
            assert_eq!(
                ids(&index.search(&query(text)).unwrap()),
                [CAVEAT_ID],
                "{text}"
            );
        }
        assert_eq!(
            ids(&index.search(&query("()\"*")).unwrap()),
            [] as [&str; 0]
        );
    }

    #[test]
    fn a_refresh_re_indexes_only_the_files_that_changed() {
        let test = TestBrain::new();
        let other = "recollections/01JB3K8V9Q0000000000000001.md";
        test.put(CAVEAT_PATH, &caveat("The shadow database is missing."));
        test.put(
            other,
            &caveat("Unchanged.").replace(CAVEAT_ID, "01JB3K8V9Q0000000000000001"),
        );
        let mut index = test.index();
        test.put(CAVEAT_PATH, &caveat("The shadow database is absent."));

        let refreshed = index.refresh(&test.brain, &test.lock()).unwrap();

        assert_eq!(
            refreshed,
            Refreshed {
                rebuilt: false,
                indexed: vec![CAVEAT_PATH.into()],
                removed: vec![],
            }
        );
        assert_eq!(ids(&index.search(&query("absent")).unwrap()), [CAVEAT_ID]);
        assert_eq!(
            ids(&index.search(&query("missing")).unwrap()),
            [] as [&str; 0]
        );
        assert_eq!(
            index.refresh(&test.brain, &test.lock()).unwrap(),
            Refreshed::default()
        );
    }

    #[test]
    fn a_refresh_drops_files_that_are_gone() {
        let test = TestBrain::new();
        test.put(CAVEAT_PATH, &caveat("The shadow database is missing."));
        let mut index = test.index();
        fs::remove_file(test.dir.path().join(CAVEAT_PATH)).unwrap();

        let refreshed = index.refresh(&test.brain, &test.lock()).unwrap();

        assert_eq!(refreshed.removed, [PathBuf::from(CAVEAT_PATH)]);
        assert_eq!(
            ids(&index.search(&query("missing")).unwrap()),
            [] as [&str; 0]
        );
    }

    #[test]
    fn another_schema_version_rebuilds_the_index_inside_the_same_file() {
        use std::os::unix::fs::MetadataExt;

        let test = TestBrain::new();
        test.put(CAVEAT_PATH, &caveat("The shadow database is missing."));
        let mut index = test.index();
        index
            .connection
            .execute(
                "UPDATE meta SET value = '0' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        let file = test.dir.path().join(FILE);
        let inode = fs::metadata(&file).unwrap().ino();
        let other = Index::open(&test.brain).unwrap();

        let refreshed = index.refresh(&test.brain, &test.lock()).unwrap();

        assert!(refreshed.rebuilt);
        assert_eq!(refreshed.indexed, [PathBuf::from(CAVEAT_PATH)]);
        assert_eq!(fs::metadata(&file).unwrap().ino(), inode);
        assert_eq!(ids(&other.search(&query("missing")).unwrap()), [CAVEAT_ID]);
        assert!(!index.refresh(&test.brain, &test.lock()).unwrap().rebuilt);
    }

    #[test]
    fn filters_narrow_the_results_to_kinds_entities_and_tags() {
        let test = TestBrain::new();
        test.put(CAVEAT_PATH, &caveat("The shadow database is missing."));
        let decision = "01JB3K8V9Q0000000000000002";
        test.put(
            &format!("recollections/{decision}.md"),
            &caveat("## Context\n\nThe shadow database.\n\n## Decision\n\n## Why\n")
                .replace(CAVEAT_ID, decision)
                .replace("kind: caveat", "kind: decision")
                .replace("- migrations", "- tooling")
                .replace("dep:npm:prisma", "dep:npm:drizzle"),
        );
        let index = test.index();
        let filtered = |change: fn(&mut Query)| {
            let mut query = query("shadow");
            change(&mut query);
            ids(&index.search(&query).unwrap())
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };

        assert_eq!(filtered(|_| ()).len(), 2);
        assert_eq!(filtered(|q| q.kinds = vec![Kind::Decision]), [decision]);
        assert_eq!(
            filtered(|q| q.entities = vec!["dep:npm:prisma".into()]),
            [CAVEAT_ID]
        );
        assert_eq!(filtered(|q| q.tags = vec!["tooling".into()]), [decision]);
        assert_eq!(filtered(|q| q.limit = 1).len(), 1);
    }

    #[test]
    fn abandoned_recollections_are_left_out_unless_asked_for() {
        let test = TestBrain::new();
        test.put(
            CAVEAT_PATH,
            &caveat("The shadow database is missing.").replace("state: active", "state: abandoned"),
        );
        let index = test.index();
        let mut abandoned = query("shadow");
        abandoned.include_abandoned = true;

        assert_eq!(
            ids(&index.search(&query("shadow")).unwrap()),
            [] as [&str; 0]
        );
        assert_eq!(ids(&index.search(&abandoned).unwrap()), [CAVEAT_ID]);
    }

    #[test]
    fn an_entity_is_found_by_its_name_and_description() {
        let test = TestBrain::new();
        test.put(
            "entities/dependencies/npm/prisma.md",
            "---\nid: dep:npm:prisma\necosystem: npm\nname: prisma\ndocs: []\nlinks: []\n---\n\nThe Prisma ORM.\n",
        );

        let hits = test.index().search(&query("orm")).unwrap();

        assert_eq!(hits.len(), 1);
        assert_eq!(
            (
                hits[0].id.as_str(),
                hits[0].kind,
                hits[0].title.as_str(),
                hits[0].summary.as_str()
            ),
            (
                "dep:npm:prisma",
                ItemKind::Dependency,
                "prisma",
                "The Prisma ORM."
            )
        );
    }

    #[test]
    fn a_file_that_doesnt_parse_is_not_found_and_not_read_again_until_it_changes() {
        let test = TestBrain::new();
        test.put(CAVEAT_PATH, "no front-matter, shadow\n");
        let mut index = test.index();

        assert_eq!(
            ids(&index.search(&query("shadow")).unwrap()),
            [] as [&str; 0]
        );
        assert_eq!(
            index.refresh(&test.brain, &test.lock()).unwrap(),
            Refreshed::default()
        );
    }
}
