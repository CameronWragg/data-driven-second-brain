//! Recovery, the first step of `serve`'s startup sequence (SPEC §2 step 2):
//! a batch a killed `serve` left half-done is rolled forward, then any other
//! change in the brain folder is committed as the owner's hand edit (§10).

use std::path::Path;
use std::time::Duration;

use crate::git::{self, GitError};
use crate::id::ItemId;
use crate::journal::{self, JournalError};
use crate::lock::{LockError, WriteLock};
use crate::write;

/// Why recovery didn't finish.
#[derive(Debug, thiserror::Error)]
pub enum RecoverError {
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Git(#[from] GitError),
}

/// Recovers the brain at `root` under the write lock, waiting for it up to
/// `lock_timeout`.
pub fn recover(root: &Path, lock_timeout: Duration) -> Result<(), RecoverError> {
    let lock = WriteLock::take(root, lock_timeout)?;
    journal::roll_forward(root, &lock)?;
    commit_hand_edits(root, &lock)?;
    Ok(())
}

/// Commits every uncommitted change in the brain at `root` with `Author:
/// owner`, if there is any. The caller holds the lock, and has rolled any
/// journal forward, so nothing left is a batch's.
fn commit_hand_edits(root: &Path, _lock: &WriteLock) -> Result<(), GitError> {
    git::run(root, &["add", "--all"])?;
    let changed = git::run(
        root,
        &["diff", "--cached", "--name-only", "--no-renames", "-z"],
    )?;
    let paths: Vec<&str> = changed
        .split('\0')
        .filter(|path| !path.is_empty())
        .collect();
    if paths.is_empty() {
        return Ok(());
    }
    let ids: Vec<ItemId> = paths
        .iter()
        .filter_map(|path| ItemId::from_file(path))
        .collect();
    let files = match paths.len() {
        1 => "1 file".to_owned(),
        count => format!("{count} files"),
    };
    let message = format!(
        "Commit hand edits to {files}\n\n{}Action: hand-edit\nAuthor: owner\n",
        write::item_trailers(&ids)
    );
    git::run(root, &["commit", "--quiet", "--message", &message])?;
    Ok(())
}
