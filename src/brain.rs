//! The brain folder (SPEC §3) and its `brain.toml` (SPEC §17.2).

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::git::{self, GitError};

/// The brain format this binary writes.
pub const FORMAT: u32 = 1;

/// Whether this binary is a debug or a release build, which decides whether
/// it needs the dev marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    Debug,
    Release,
}

impl Build {
    /// The build this binary is.
    pub const CURRENT: Build = if cfg!(debug_assertions) {
        Build::Debug
    } else {
        Build::Release
    };
}

/// `brain.toml` as committed in the brain folder.
#[derive(Debug, Deserialize)]
struct BrainToml {
    format: u32,
    #[serde(default)]
    dev: bool,
}

/// An open brain folder.
#[derive(Debug)]
pub struct Brain {
    root: PathBuf,
    format: u32,
}

/// Why a brain folder couldn't be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{} isn't a Second Brain: it has no brain.toml", root.display())]
    NotABrain { root: PathBuf },
    #[error("couldn't read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("{} is invalid: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: toml_edit::de::Error,
    },
    #[error(
        "{} has no `dev = true` in its brain.toml, and a debug build only opens a dev brain; \
         use the `just dev-*` recipes, or a release build for a real brain",
        root.display()
    )]
    NotDev { root: PathBuf },
}

/// The folders of a brain's layout (SPEC §3).
const LAYOUT: [&str; 5] = [
    "recollections",
    "entities/repositories",
    "entities/dependencies",
    "eval",
    ".index",
];

/// Why a brain folder couldn't be created.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("{} isn't empty; init creates a brain only in a new or empty folder", root.display())]
    NotEmpty { root: PathBuf },
    #[error("couldn't create the brain at {}: {source}", root.display())]
    Io { root: PathBuf, source: io::Error },
    #[error(transparent)]
    Git(#[from] GitError),
}

/// The first commit's message (SPEC §5.3).
const FIRST_COMMIT: &str = "Create the Second Brain\n\nAuthor: server\n";

/// Fills the empty or missing folder `root` with a new brain.
fn populate(root: &Path, build: Build) -> Result<(), CreateError> {
    let io = |source| CreateError::Io {
        root: root.into(),
        source,
    };
    for dir in LAYOUT {
        std::fs::create_dir_all(root.join(dir)).map_err(io)?;
    }
    let mut brain_toml = format!("format = {FORMAT}\n");
    if build == Build::Debug {
        brain_toml.push_str("dev = true\n");
    }
    std::fs::write(root.join("brain.toml"), brain_toml).map_err(io)?;
    std::fs::write(root.join(".gitignore"), "/.index/\n").map_err(io)?;
    git::run(root, &["init", "--quiet"])?;
    git::run(root, &["add", ".gitignore", "brain.toml"])?;
    git::run(root, &["commit", "--quiet", "--message", FIRST_COMMIT])?;
    Ok(())
}

impl Brain {
    /// Creates a new brain folder at `root`, with its layout and `brain.toml`,
    /// as a git repository holding one commit.
    pub fn create(root: &Path) -> Result<Brain, CreateError> {
        Brain::create_as(root, Build::CURRENT)
    }

    fn create_as(root: &Path, build: Build) -> Result<Brain, CreateError> {
        let existed = match std::fs::read_dir(root) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(CreateError::NotEmpty { root: root.into() });
                }
                true
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(source) => {
                return Err(CreateError::Io {
                    root: root.into(),
                    source,
                });
            }
        };
        if let Err(error) = populate(root, build) {
            // Leave the folder as it was found, so `init` can run again.
            if existed {
                for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
                    let path = entry.path();
                    let _ = std::fs::remove_dir_all(&path).or_else(|_| std::fs::remove_file(&path));
                }
            } else {
                let _ = std::fs::remove_dir_all(root);
            }
            return Err(error);
        }
        Ok(Brain {
            root: root.into(),
            format: FORMAT,
        })
    }

    /// Opens the brain folder at `root`.
    pub fn open(root: &Path) -> Result<Brain, OpenError> {
        Brain::open_as(root, Build::CURRENT)
    }

    fn open_as(root: &Path, build: Build) -> Result<Brain, OpenError> {
        let path = root.join("brain.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(OpenError::NotABrain { root: root.into() });
            }
            Err(source) => return Err(OpenError::Read { path, source }),
        };
        let brain_toml: BrainToml =
            toml_edit::de::from_str(&text).map_err(|source| OpenError::Parse { path, source })?;
        if build == Build::Debug && !brain_toml.dev {
            return Err(OpenError::NotDev { root: root.into() });
        }
        Ok(Brain {
            root: root.into(),
            format: brain_toml.format,
        })
    }

    /// The brain folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The format version in its `brain.toml`.
    pub fn format(&self) -> u32 {
        self.format
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn brain_with(brain_toml: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("brain.toml"), brain_toml).unwrap();
        root
    }

    #[test]
    fn a_debug_build_refuses_a_brain_without_the_dev_marker() {
        let root = brain_with("format = 1\n");

        let error = Brain::open_as(root.path(), Build::Debug).unwrap_err();

        assert!(matches!(error, OpenError::NotDev { .. }), "{error}");
    }

    #[test]
    fn a_debug_build_opens_a_dev_brain() {
        let root = brain_with("format = 1\ndev = true\n");

        let brain = Brain::open_as(root.path(), Build::Debug).unwrap();

        assert_eq!(brain.root(), root.path());
        assert_eq!(brain.format(), 1);
    }

    #[test]
    fn a_release_build_opens_a_brain_without_the_dev_marker() {
        let root = brain_with("format = 1\n");

        Brain::open_as(root.path(), Build::Release).unwrap();
    }

    #[test]
    fn a_folder_without_brain_toml_is_not_a_brain() {
        let root = tempfile::tempdir().unwrap();

        let error = Brain::open_as(root.path(), Build::Release).unwrap_err();

        assert!(matches!(error, OpenError::NotABrain { .. }), "{error}");
    }
}
