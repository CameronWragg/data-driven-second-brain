//! The write journal, `.index/journal` (SPEC §5.3): a batch's whole outcome,
//! written and fsynced before any file is touched, so a batch that `serve`
//! was killed part-way through is rolled forward with its own message.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::{self, GitError};
use crate::lock::WriteLock;

/// The journal, relative to the brain folder.
const FILE: &str = ".index/journal";

/// Where the journal is written before it's renamed into place, so a kill
/// never leaves half a journal behind.
const PARTIAL: &str = ".index/journal.partial";

/// A batch about to be committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// HEAD before the batch. Once HEAD has moved on, the batch has landed.
    parent: String,
    message: String,
    files: Vec<JournalFile>,
}

/// One file a batch changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalFile {
    /// Relative to the brain folder, with `/` between its parts.
    path: String,
    /// The file's whole new text, or `None` to delete it.
    text: Option<String>,
}

/// Why the journal couldn't be written or rolled forward.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("couldn't write {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("the write journal {} can't be read ({source}); the owner must look at it and delete it", path.display())]
    Unreadable {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error(transparent)]
    Git(#[from] GitError),
}

impl Journal {
    /// A journal for a batch committed on top of HEAD `parent` with
    /// `message`, which leaves each path with its text, or deletes it.
    pub fn new<'a>(
        parent: &str,
        message: &str,
        files: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    ) -> Journal {
        Journal {
            parent: parent.to_owned(),
            message: message.to_owned(),
            files: files
                .into_iter()
                .map(|(path, text)| JournalFile {
                    path: path.to_owned(),
                    text: text.map(str::to_owned),
                })
                .collect(),
        }
    }

    /// Writes the journal into the brain at `root` and fsyncs it.
    pub fn record(&self, root: &Path, _lock: &WriteLock) -> Result<(), JournalError> {
        let partial = root.join(PARTIAL);
        let io = |source| JournalError::Io {
            path: partial.clone(),
            source,
        };
        let json = serde_json::to_vec(self).expect("a journal always serialises");
        let mut file = File::create(&partial).map_err(io)?;
        file.write_all(&json).map_err(io)?;
        file.sync_all().map_err(io)?;
        fs::rename(&partial, root.join(FILE)).map_err(io)?;
        // The rename is durable only once its folder is.
        File::open(root.join(".index"))
            .and_then(|dir| dir.sync_all())
            .map_err(io)
    }

    /// Rewrites every file and commits them with the journal's message,
    /// unless HEAD has moved on from `parent`, so the commit already landed.
    fn replay(&self, root: &Path) -> Result<(), JournalError> {
        if git::run(root, &["rev-parse", "HEAD"])?.trim() != self.parent {
            return Ok(());
        }
        for file in &self.files {
            put(root, Path::new(&file.path), file.text.as_deref()).map_err(|source| {
                JournalError::Io {
                    path: root.join(&file.path),
                    source,
                }
            })?;
        }
        let paths: Vec<&str> = self.files.iter().map(|file| file.path.as_str()).collect();
        // `add` stages a deleted path's removal too, and fails on a path git
        // has never had, which a replay never names.
        let mut add = vec!["add", "--"];
        add.extend(&paths);
        git::run(root, &add)?;
        let mut staged = vec!["diff", "--cached", "--name-only", "--"];
        staged.extend(&paths);
        if git::run(root, &staged)?.trim().is_empty() {
            return Ok(());
        }
        let mut commit = vec!["commit", "--quiet", "--message", &self.message, "--"];
        commit.extend(&paths);
        git::run(root, &commit)?;
        Ok(())
    }
}

/// Deletes the journal of the brain at `root`, if there is one.
pub fn clear(root: &Path, _lock: &WriteLock) -> Result<(), JournalError> {
    let path = root.join(FILE);
    match fs::remove_file(&path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(JournalError::Io {
            path,
            source: error,
        }),
        _ => Ok(()),
    }
}

/// Rolls forward a journal left in the brain at `root`: rewrites every file
/// from it and commits them with its message, unless its commit already
/// landed, then deletes it. Returns whether there was one.
pub fn roll_forward(root: &Path, lock: &WriteLock) -> Result<bool, JournalError> {
    let path = root.join(FILE);
    let json = match fs::read(&path) {
        Ok(json) => json,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(JournalError::Io { path, source }),
    };
    let journal: Journal = serde_json::from_slice(&json)
        .map_err(|source| JournalError::Unreadable { path, source })?;
    journal.replay(root)?;
    clear(root, lock)?;
    Ok(true)
}

/// Writes `text` to `path` under `root`, making its folders, or removes the
/// file if `text` is `None`.
pub fn put(root: &Path, path: &Path, text: Option<&str>) -> io::Result<()> {
    let full = root.join(path);
    match text {
        Some(text) => {
            if let Some(dir) = full.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&full, text)
        }
        None => match fs::remove_file(&full) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const PATH: &str = "recollections/01JB3K8V9Q0000000000000000.md";
    const MESSAGE: &str = "Create caveat: Prisma migrations fail silently\n\n\
                           Recollection: 01JB3K8V9Q0000000000000000\n\
                           Action: create\nAuthor: agent\n";

    struct TestBrain {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl TestBrain {
        fn new() -> TestBrain {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("brain");
            fs::create_dir_all(root.join(".index")).unwrap();
            // A brain's repository made by hand rather than by `init`, so
            // its first commit has an author where git has no global one.
            git::run(&root, &["init", "--quiet"]).unwrap();
            for (key, value) in [
                ("user.name", "Test Owner"),
                ("user.email", "owner@example.com"),
            ] {
                git::run(&root, &["config", key, value]).unwrap();
            }
            fs::write(root.join(".gitignore"), "/.index/\n").unwrap();
            git::run(&root, &["add", ".gitignore"]).unwrap();
            git::run(&root, &["commit", "--quiet", "--message", "Start"]).unwrap();
            TestBrain { _dir: dir, root }
        }

        fn lock(&self) -> WriteLock {
            WriteLock::take(&self.root, Duration::ZERO).unwrap()
        }

        fn git(&self, args: &[&str]) -> String {
            git::run(&self.root, args).unwrap()
        }

        fn head(&self) -> String {
            self.git(&["rev-parse", "HEAD"]).trim().to_owned()
        }
    }

    #[test]
    fn a_journal_left_before_its_commit_is_committed_with_its_own_message() {
        let test = TestBrain::new();
        let lock = test.lock();
        let journal = Journal::new(&test.head(), MESSAGE, [(PATH, Some("caveat\n"))]);
        journal.record(&test.root, &lock).unwrap();

        assert!(roll_forward(&test.root, &lock).unwrap());

        assert_eq!(
            test.git(&["log", "-1", "--format=%B"]).trim_end(),
            MESSAGE.trim_end()
        );
        assert_eq!(
            fs::read_to_string(test.root.join(PATH)).unwrap(),
            "caveat\n"
        );
        assert_eq!(test.git(&["status", "--porcelain"]), "");
        assert!(!test.root.join(FILE).exists());
    }

    #[test]
    fn replaying_a_journal_twice_gives_the_same_result() {
        let test = TestBrain::new();
        let lock = test.lock();
        fs::create_dir_all(test.root.join("recollections")).unwrap();
        fs::write(test.root.join(PATH), "old\n").unwrap();
        test.git(&["add", "."]);
        test.git(&["commit", "--quiet", "--message", "Add the old file"]);
        let journal = Journal::new(
            &test.head(),
            MESSAGE,
            [
                (PATH, None),
                ("recollections/01JB3M0000000000000000000A.md", Some("new\n")),
            ],
        );
        journal.record(&test.root, &lock).unwrap();
        // Killed part-way through writing the files.
        fs::remove_file(test.root.join(PATH)).unwrap();

        roll_forward(&test.root, &lock).unwrap();
        let once = (
            test.head(),
            test.git(&["show", "--stat", "--format=%B", "HEAD"]),
        );
        // As if killed again after the commit but before the journal was deleted.
        journal.record(&test.root, &lock).unwrap();
        assert!(roll_forward(&test.root, &lock).unwrap());

        assert_eq!(
            (
                test.head(),
                test.git(&["show", "--stat", "--format=%B", "HEAD"])
            ),
            once
        );
        assert_eq!(test.git(&["rev-list", "--count", "HEAD"]), "3\n");
        assert_eq!(test.git(&["status", "--porcelain"]), "");
        assert!(!test.root.join(FILE).exists());
    }

    #[test]
    fn with_no_journal_nothing_is_rolled_forward() {
        let test = TestBrain::new();
        let head = test.head();

        assert!(!roll_forward(&test.root, &test.lock()).unwrap());

        assert_eq!(test.head(), head);
    }
}
