//! Checkouts (SPEC §4.3): the local copy of a Repository an Agent works in,
//! and the Repository and Branch it gives.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ulid::Ulid;

use crate::brain::Brain;
use crate::entity::{Entity, Fields};
use crate::git::{self, GitError};
use crate::id::EntityId;
use crate::index::Index;
use crate::lock::WriteLock;
use crate::write::{self, Change, WriteError};

/// A Checkout found on this machine, and what it gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// The Checkout's top-level folder.
    pub path: PathBuf,
    /// The Repository it is a copy of.
    pub repository: EntityId,
    /// What the Repository is called when the brain first records it.
    pub name: String,
    /// The Branch checked out, or being rebased; `None` if HEAD is detached
    /// with no rebase to name one.
    pub branch: Option<String>,
    /// The Repository's default branch; `None` if nothing in the Checkout or
    /// the brain says which it is.
    pub default_branch: Option<String>,
    /// An `origin` that isn't a hosted remote, which is why the Repository
    /// has a local ID.
    pub unhosted_remote: Option<String>,
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
    // `get-url`, unlike reading the config, applies `insteadOf` rewrites.
    let remote = git::run(&path, &["remote", "get-url", "origin"])
        .ok()
        .map(|url| url.trim().to_owned());
    let (repository, name, unhosted_remote) = match remote.as_deref().and_then(repository_of_remote)
    {
        Some(repository) => {
            let name = repository
                .to_string()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            (repository, name, None)
        }
        None => {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            (local_repository(&path)?, name, remote)
        }
    };
    let branch = current_branch(&path);
    let default_branch = default_branch(&path);
    Ok(Some(Checkout {
        path,
        repository,
        name,
        branch,
        default_branch,
        unhosted_remote,
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
    // Made only if missing, so two `serve`s starting together agree on one ID.
    match OpenOptions::new().write(true).create_new(true).open(&file) {
        Ok(mut created) => created
            .write_all(format!("{}\n", Ulid::generate()).as_bytes())
            .map_err(io)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => return Err(io(source)),
    }
    let text = std::fs::read_to_string(&file).map_err(io)?;
    let ulid = Ulid::from_string(text.trim()).map_err(|_| DetectError::BadLocalId(file.clone()))?;
    Ok(EntityId::Repository {
        path: format!("local/{ulid}"),
    })
}

/// The Branch checked out at `path`, or the one a rebase in progress is
/// rebasing, since a rebase detaches HEAD; `None` if neither names one.
fn current_branch(path: &Path) -> Option<String> {
    if let Ok(branch) = git::run(path, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        return Some(branch.trim().to_owned());
    }
    ["rebase-merge/head-name", "rebase-apply/head-name"]
        .into_iter()
        .find_map(|name| {
            let file = git::run(path, &["rev-parse", "--git-path", name]).ok()?;
            let head = std::fs::read_to_string(path.join(file.trim())).ok()?;
            head.trim().strip_prefix("refs/heads/").map(str::to_owned)
        })
}

/// The default branch as the Checkout at `path` knows it: the remote's
/// HEAD, else a local `main` or `master`, else its only local Branch.
fn default_branch(path: &Path) -> Option<String> {
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
        return Some(branch.to_owned());
    }
    for branch in ["main", "master"] {
        let local = format!("refs/heads/{branch}");
        if git::run(path, &["rev-parse", "--verify", "--quiet", &local]).is_ok() {
            return Some(branch.to_owned());
        }
    }
    let branches = git::run(
        path,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )
    .ok()?;
    match branches.lines().collect::<Vec<_>>().as_slice() {
        [only] => Some((*only).to_owned()),
        _ => None,
    }
}

impl Checkout {
    /// Reads the Branch checked out again, since it may have changed since
    /// the Checkout was detected.
    pub fn refresh_branch(&mut self) {
        self.branch = current_branch(&self.path);
    }

    /// The `branch` a Recollection written here records: `None` on the
    /// default branch, else the Branch's ID (SPEC §4.1). Also `None` on a
    /// detached HEAD, where no Recollection can be written.
    pub fn branch_id(&self) -> Option<String> {
        let branch = self.branch.as_deref()?;
        (self.default_branch.as_deref() != Some(branch)).then(|| self.repository.branch(branch))
    }
}

/// Makes sure the brain has an Entity for the Checkout's Repository, creating
/// it in a commit with `Author: server` if not (SPEC §2 step 4), and records
/// the Checkout in the index. A Repository the brain already has gives the
/// Checkout its `default_branch`, or takes the Checkout's if it had none.
/// Returns whether the index took every change.
pub fn settle(
    brain: &Brain,
    index: &mut Index,
    lock_timeout: Duration,
    checkout: &mut Checkout,
) -> Result<bool, WriteError> {
    let mut indexed = true;
    let full = brain.root().join(checkout.repository.file());
    let known = std::fs::read_to_string(&full)
        .ok()
        .and_then(|text| repository_fields(checkout, &text));
    match known.and_then(|fields| fields.default_branch) {
        Some(default_branch) => checkout.default_branch = Some(default_branch),
        None => {
            let lock = WriteLock::take(brain.root(), lock_timeout)?;
            // Read again under the lock: another `serve` may have written it.
            let text = std::fs::read_to_string(&full).ok();
            indexed = match text {
                None => create_repository(brain, index, &lock, checkout)?,
                Some(text) => match repository_fields(checkout, &text) {
                    Some(fields) => match fields.default_branch {
                        Some(default_branch) => {
                            checkout.default_branch = Some(default_branch);
                            true
                        }
                        None => learn_default_branch(brain, index, &lock, checkout, &text)?,
                    },
                    // A broken file is the owner's to repair (SPEC §10).
                    None => true,
                },
            };
        }
    }
    index.record_checkout(&checkout.path, &checkout.repository)?;
    Ok(indexed)
}

/// The front-matter of the Checkout's Repository file `text`, if it parses.
fn repository_fields(checkout: &Checkout, text: &str) -> Option<crate::entity::Repository> {
    match Entity::parse(&checkout.repository, text).ok()?.fields {
        Fields::Repository(fields) => Some(fields),
        Fields::Dependency(_) => None,
    }
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
        fields.default_branch = checkout.default_branch.clone();
    }
    let change = Change::CreatedEntity { reference: None };
    Ok(write::by_server(brain, index, lock, change, id, entity)?.indexed)
}

/// Records the Checkout's default branch in a Repository whose file `text`
/// has none, if the Checkout knows it.
fn learn_default_branch(
    brain: &Brain,
    index: &mut Index,
    lock: &WriteLock,
    checkout: &Checkout,
    text: &str,
) -> Result<bool, WriteError> {
    let id = &checkout.repository;
    let (Some(default_branch), Ok(mut entity)) =
        (&checkout.default_branch, Entity::parse(id, text))
    else {
        return Ok(true);
    };
    if let Fields::Repository(fields) = &mut entity.fields {
        fields.default_branch = Some(default_branch.clone());
    }
    Ok(write::by_server(brain, index, lock, Change::UpdatedEntity, id, entity)?.indexed)
}

/// The Repository ID of the remote at `url`: its host lower-cased, `.git`
/// removed from the end, and SSH and HTTPS forms brought to one
/// `host/owner/name` form, or a deeper path such as a GitLab subgroup's
/// (SPEC §4.1). `None` if `url` isn't a hosted remote with at least an owner
/// and a name, such as a path on this machine.
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
    fn a_remote_with_a_deeper_path_keeps_all_of_it() {
        assert_eq!(
            repository_of_remote("git@gitlab.com:group/subgroup/api.git").map(|id| id.to_string()),
            Some("repo:gitlab.com/group/subgroup/api".to_owned())
        );
    }

    #[test]
    fn remotes_without_an_owner_and_a_name_have_no_repository_id() {
        for url in [
            "",
            "/home/owner/api",
            "../api.git",
            "file:///srv/git/api.git",
            "https://github.com/api",
            "git@github.com:",
        ] {
            assert_eq!(repository_of_remote(url), None, "{url}");
        }
    }
}
