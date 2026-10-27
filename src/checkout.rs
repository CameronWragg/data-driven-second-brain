//! Checkouts (SPEC §4.3): the local copy of a Repository an Agent works in,
//! and the Repository and Branch it gives.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ulid::Ulid;

use crate::brain::Brain;
use crate::entity::{Entity, Fields};
use crate::git::{self, GitError};
use crate::id::{EntityId, ItemId};
use crate::index::Index;
use crate::lock::WriteLock;
use crate::write::{self, FileChange, WriteError};

/// A Checkout found on this machine, and what it gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// The Checkout's top-level folder.
    pub path: PathBuf,
    pub repository: EntityId,
    /// What the Repository is called when the brain first records it.
    pub name: String,
    /// The Branch checked out, or `None` on a detached HEAD.
    pub branch: Option<String>,
    pub default_branch: String,
}

/// Why a folder's Checkout couldn't be detected.
#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error("{} isn't a folder", .0.display())]
    NotAFolder(PathBuf),
    #[error("couldn't read or write {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{} doesn't hold a Repository ID; delete it to have a new one made", .0.display())]
    BadLocalId(PathBuf),
    #[error(transparent)]
    Git(#[from] GitError),
}

/// The file inside a Checkout's git folder that holds the ID of a Repository
/// with no remote (SPEC §4.3).
const LOCAL_ID_FILE: &str = "latens-id";

/// The Checkout `dir` is in, or `None` if it is in none, or only in the brain
/// folder at `brain_root`, which is never a Checkout.
pub fn detect(dir: &Path, brain_root: &Path) -> Result<Option<Checkout>, DetectError> {
    if !dir.is_dir() {
        return Err(DetectError::NotAFolder(dir.into()));
    }
    let Ok(top) = git::run(dir, &["rev-parse", "--show-toplevel"]) else {
        return Ok(None);
    };
    let path = PathBuf::from(top.trim());
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.into());
    if canonical(&path).starts_with(canonical(brain_root)) {
        return Ok(None);
    }
    let remote = git::run(&path, &["config", "--get", "remote.origin.url"]).ok();
    let (repository, name) = match remote
        .as_deref()
        .and_then(|url| repository_of_remote(url.trim()))
    {
        Some(repository) => {
            let name = repository
                .to_string()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            (repository, name)
        }
        None => {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            (local_repository(&path)?, name)
        }
    };
    let branch = current_branch(&path);
    let default_branch = default_branch(&path, branch.as_deref());
    Ok(Some(Checkout {
        path,
        repository,
        name,
        branch,
        default_branch,
    }))
}

/// The ID of the Repository with no remote checked out at `path`, made and
/// recorded in its git folder the first time.
fn local_repository(path: &Path) -> Result<EntityId, DetectError> {
    // The common folder, so every worktree of the Checkout shares one ID.
    let git_dir = PathBuf::from(git::run(path, &["rev-parse", "--git-common-dir"])?.trim());
    let file = path.join(git_dir).join(LOCAL_ID_FILE);
    let io = |source| DetectError::Io {
        path: file.clone(),
        source,
    };
    let ulid = match std::fs::read_to_string(&file) {
        Ok(text) => {
            Ulid::from_string(text.trim()).map_err(|_| DetectError::BadLocalId(file.clone()))?
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let ulid = Ulid::generate();
            std::fs::write(&file, format!("{ulid}\n")).map_err(io)?;
            ulid
        }
        Err(source) => return Err(io(source)),
    };
    Ok(EntityId::Repository {
        path: format!("local/{ulid}"),
    })
}

/// The Branch checked out at `path`, or `None` on a detached HEAD.
fn current_branch(path: &Path) -> Option<String> {
    git::run(path, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|branch| branch.trim().to_owned())
}

/// The default branch as the Checkout at `path` knows it: the remote's
/// HEAD, else a local `main` or `master`, else the current Branch.
fn default_branch(path: &Path, current: Option<&str>) -> String {
    if let Ok(head) = git::run(
        path,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    ) && let Some(branch) = head.trim().strip_prefix("origin/")
    {
        return branch.to_owned();
    }
    for branch in ["main", "master"] {
        let local = format!("refs/heads/{branch}");
        if git::run(path, &["rev-parse", "--verify", "--quiet", &local]).is_ok() {
            return branch.to_owned();
        }
    }
    current.unwrap_or("main").to_owned()
}

impl Checkout {
    /// Reads the Branch checked out again, since it may have changed since
    /// the Checkout was detected.
    pub fn refresh_branch(&mut self) {
        self.branch = current_branch(&self.path);
    }

    /// The `branch` a Recollection written here records: `None` on the
    /// default branch, and on a detached HEAD, which is on no Branch.
    pub fn recorded_branch(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .filter(|branch| *branch != self.default_branch)
    }
}

/// Makes sure the brain has an Entity for the Checkout's Repository, creating
/// it in a commit with `Author: server` if not (SPEC §2 step 4), and records
/// the Checkout in the index. A Repository the brain already has gives the
/// Checkout its `default_branch`. Returns whether the index took the change.
pub fn settle(
    brain: &Brain,
    index: &mut Index,
    lock_timeout: Duration,
    checkout: &mut Checkout,
) -> Result<bool, WriteError> {
    let mut indexed = true;
    let file = checkout.repository.file();
    let full = brain.root().join(&file);
    let existing = std::fs::read_to_string(&full).ok();
    let existing = match existing {
        Some(text) => Some(text),
        None => {
            let lock = WriteLock::take(brain.root(), lock_timeout)?;
            // Another `serve` may have made it while this one waited.
            match std::fs::read_to_string(&full) {
                Ok(text) => Some(text),
                Err(_) => {
                    indexed = create_repository(brain, index, &lock, checkout)?;
                    None
                }
            }
        }
    };
    if let Some(text) = existing
        && let Ok(Entity {
            fields: Fields::Repository(fields),
            ..
        }) = Entity::parse(&checkout.repository, &text)
        && let Some(default_branch) = fields.default_branch
    {
        checkout.default_branch = default_branch;
    }
    index.record_checkout(&checkout.path, &checkout.repository.to_string())?;
    Ok(indexed)
}

/// Commits a new Repository Entity for `checkout`.
fn create_repository(
    brain: &Brain,
    index: &mut Index,
    lock: &WriteLock,
    checkout: &Checkout,
) -> Result<bool, WriteError> {
    let id = &checkout.repository;
    let mut entity = Entity::new(id, &checkout.name, "");
    if let Fields::Repository(fields) = &mut entity.fields {
        fields.default_branch = Some(checkout.default_branch.clone());
    }
    let message = format!(
        "Create repository: {}\n\nRepository: {id}\nAction: create_entity\nAuthor: server\n",
        checkout.name
    );
    let change = FileChange {
        path: ItemId::Entity(id.clone()).file(),
        before: None,
        after: Some(entity.render()),
    };
    let (_, indexed) = write::commit(brain.root(), index, lock, &[change], &message)?;
    Ok(indexed)
}

/// The Repository ID of the remote at `url`: its host lower-cased, `.git`
/// removed from the end, and SSH and HTTPS forms brought to one
/// `host/owner/name` form (SPEC §4.1). `None` if `url` isn't a remote with
/// exactly an owner and a name.
pub fn repository_of_remote(url: &str) -> Option<EntityId> {
    let (host, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            if !["ssh", "git", "http", "https", "git+ssh", "ssh+git"].contains(&scheme) {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            // A port, never part of the Repository's identity.
            let host = host.split(':').next()?;
            (host, path)
        }
        // scp-like SSH: `[user@]host:owner/name`.
        None => {
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            (authority.rsplit('@').next()?, path)
        }
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || host == "local" {
        return None;
    }
    match EntityId::parse(&format!("repo:{}/{path}", host.to_lowercase()))? {
        repository @ EntityId::Repository { .. } => Some(repository),
        EntityId::Dependency { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_and_https_remotes_of_one_repository_get_the_same_id() {
        for url in [
            "git@github.com:CameronWragg/api.git",
            "git@GitHub.com:CameronWragg/api",
            "ssh://git@github.com/CameronWragg/api.git",
            "ssh://git@github.com:22/CameronWragg/api",
            "https://github.com/CameronWragg/api.git",
            "https://user:token@GITHUB.COM/CameronWragg/api/",
            "http://github.com/CameronWragg/api",
            "git://github.com/CameronWragg/api.git",
        ] {
            assert_eq!(
                repository_of_remote(url).map(|id| id.to_string()),
                Some("repo:github.com/CameronWragg/api".to_owned()),
                "{url}"
            );
        }
    }

    #[test]
    fn remotes_without_an_owner_and_a_name_have_no_repository_id() {
        for url in [
            "",
            "/home/owner/api",
            "../api.git",
            "file:///srv/git/api.git",
            "https://github.com/api",
            "https://gitlab.com/group/subgroup/api.git",
            "git@github.com:",
        ] {
            assert_eq!(repository_of_remote(url), None, "{url}");
        }
    }
}
