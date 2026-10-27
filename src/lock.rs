//! The write lock, `.index/lock`: an exclusive `flock` (SPEC §11).

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a writer waits for the lock when `lock.timeout` isn't set.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// How often a waiting writer tries the lock again.
const RETRY: Duration = Duration::from_millis(10);

/// The held lock; dropping it releases the lock.
#[derive(Debug)]
pub struct WriteLock {
    _file: File,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another writer held the write lock for longer than {}", humantime::format_duration(*.0))]
    Timeout(Duration),
    #[error("couldn't open the write lock {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

impl WriteLock {
    /// Takes the lock of the brain at `root`, waiting up to `timeout`.
    pub fn take(root: &Path, timeout: Duration) -> Result<WriteLock, LockError> {
        let path = root.join(".index/lock");
        let io = |source| LockError::Io {
            path: path.clone(),
            source,
        };
        // A cloned brain has no `.index/` until something makes it (SPEC §3).
        std::fs::create_dir_all(root.join(".index")).map_err(io)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io)?;
        let deadline = Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(WriteLock { _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(RETRY)
                }
                Err(TryLockError::WouldBlock) => return Err(LockError::Timeout(timeout)),
                Err(TryLockError::Error(source)) => return Err(io(source)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_writer_times_out_while_the_lock_is_held() {
        let root = tempfile::tempdir().unwrap();
        let held = WriteLock::take(root.path(), Duration::ZERO).unwrap();

        let error = WriteLock::take(root.path(), Duration::from_millis(30)).unwrap_err();

        assert!(matches!(error, LockError::Timeout(_)), "{error}");
        drop(held);
        WriteLock::take(root.path(), Duration::ZERO).unwrap();
    }
}
