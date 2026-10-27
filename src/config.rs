//! This machine's configuration (SPEC §17.1): an optional
//! `$XDG_CONFIG_HOME/latens/config.toml` that is never committed or synced.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer};

/// The XDG base directories Latens uses, on macOS as well as Linux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    pub config_home: PathBuf,
    pub data_home: PathBuf,
}

#[derive(Debug, thiserror::Error)]
#[error("HOME isn't set, so {0} can't default to a folder in it")]
pub struct NoHome(&'static str);

impl Dirs {
    /// Reads the directories from this process's environment.
    pub fn from_env() -> Result<Dirs, NoHome> {
        Dirs::from_vars(|name| std::env::var_os(name))
    }

    /// Reads the directories through `var`, falling back to `$HOME` for a
    /// variable that is unset, empty or relative, as the XDG spec says.
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Dirs, NoHome> {
        let dir = |name: &'static str, fallback: &str| {
            if let Some(dir) = var(name).map(PathBuf::from)
                && dir.is_absolute()
            {
                return Ok(dir);
            }
            var("HOME")
                .filter(|home| !home.is_empty())
                .map(|home| PathBuf::from(home).join(fallback))
                .ok_or(NoHome(name))
        };
        Ok(Dirs {
            config_home: dir("XDG_CONFIG_HOME", ".config")?,
            data_home: dir("XDG_DATA_HOME", ".local/share")?,
        })
    }

    /// Where this machine's configuration file is.
    pub fn config_file(&self) -> PathBuf {
        self.config_home.join("latens/config.toml")
    }

    /// Where the brain folder is when `brain.path` isn't set (SPEC §3).
    pub fn default_brain_path(&self) -> PathBuf {
        self.data_home.join("latens")
    }
}

/// Which embedding provider builds vectors (SPEC §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingProvider {
    #[default]
    Onnx,
    Hash,
}

/// This machine's configuration, with every default filled in.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub brain_path: PathBuf,
    pub embedding_provider: EmbeddingProvider,
    /// `None` means the default model.
    pub embedding_model: Option<String>,
    /// `None` means the threshold in code for the configured model.
    pub duplicates_threshold: Option<f32>,
    /// `None` means the timeout in code; likewise the next two.
    pub lock_timeout: Option<Duration>,
    pub startup_timeout: Option<Duration>,
    pub model_timeout: Option<Duration>,
    pub sync_timeout: Duration,
}

/// The default for `sync.timeout`.
const SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// Why the configuration file couldn't be read or written.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("couldn't read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("{} is invalid: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: toml_edit::de::Error,
    },
    #[error(
        "{} is invalid: brain.path must be absolute, not {}",
        path.display(),
        brain_path.display()
    )]
    RelativeBrainPath { path: PathBuf, brain_path: PathBuf },
    #[error("couldn't write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("brain.path must be UTF-8 to go in a TOML file, and {} isn't", .0.display())]
    NotUtf8(PathBuf),
}

/// `config.toml` as written; every table rejects keys it doesn't know.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConfigToml {
    brain: BrainTable,
    embedding: EmbeddingTable,
    duplicates: DuplicatesTable,
    lock: TimeoutTable,
    startup: TimeoutTable,
    model: TimeoutTable,
    sync: TimeoutTable,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct BrainTable {
    path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct EmbeddingTable {
    provider: EmbeddingProvider,
    model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DuplicatesTable {
    threshold: Option<f32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TimeoutTable {
    #[serde(deserialize_with = "duration")]
    timeout: Option<Duration>,
}

/// Reads a duration string such as `"200ms"` or `"1m 30s"`.
fn duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
    let text = String::deserialize(deserializer)?;
    humantime::parse_duration(&text)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

impl Config {
    /// Loads `config.toml` from `dirs`; a missing file means every default.
    pub fn load(dirs: &Dirs) -> Result<Config, ConfigError> {
        let path = dirs.config_file();
        let file = match std::fs::read_to_string(&path) {
            Ok(text) => toml_edit::de::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.clone(),
                source,
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => ConfigToml::default(),
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        let brain_path = match file.brain.path {
            Some(brain_path) if brain_path.is_relative() => {
                return Err(ConfigError::RelativeBrainPath { path, brain_path });
            }
            Some(brain_path) => brain_path,
            None => dirs.default_brain_path(),
        };
        Ok(Config {
            brain_path,
            embedding_provider: file.embedding.provider,
            embedding_model: file.embedding.model,
            duplicates_threshold: file.duplicates.threshold,
            lock_timeout: file.lock.timeout,
            startup_timeout: file.startup.timeout,
            model_timeout: file.model.timeout,
            sync_timeout: file.sync.timeout.unwrap_or(SYNC_TIMEOUT),
        })
    }
}

/// Points the configuration file at the brain folder `brain_path`, creating
/// the file if need be and keeping everything else in it as it was. A brain
/// at the default path removes `brain.path`, so no key means the default.
pub fn set_brain_path(dirs: &Dirs, brain_path: &Path) -> Result<(), ConfigError> {
    let path = dirs.config_file();
    let value = brain_path
        .to_str()
        .ok_or_else(|| ConfigError::NotUtf8(brain_path.into()))?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(source) => return Err(ConfigError::Read { path, source }),
    };
    let mut document: toml_edit::DocumentMut =
        text.parse()
            .map_err(|source: toml_edit::TomlError| ConfigError::Parse {
                path: path.clone(),
                source: source.into(),
            })?;
    if same_folder(brain_path, &dirs.default_brain_path()) {
        if let Some(brain) = document
            .get_mut("brain")
            .and_then(|item| item.as_table_like_mut())
        {
            brain.remove("path");
            if brain.is_empty() {
                document.remove("brain");
            }
        }
    } else {
        document.entry("brain").or_insert_with(toml_edit::table)["path"] = toml_edit::value(value);
    }
    let write = |source| ConfigError::Write {
        path: path.clone(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(write)?;
    }
    std::fs::write(&path, document.to_string()).map_err(write)
}

/// Whether two paths name the same folder, following symlinks where the
/// folders exist (macOS's `/var` is itself a symlink).
pub(crate) fn same_folder(a: &Path, b: &Path) -> bool {
    let resolved = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
    resolved(a) == resolved(b)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::time::Duration;

    use super::*;

    fn dirs(root: &Path) -> Dirs {
        Dirs {
            config_home: root.join("config"),
            data_home: root.join("data"),
        }
    }

    #[test]
    fn a_missing_file_means_every_default() {
        let root = tempfile::tempdir().unwrap();

        let config = Config::load(&dirs(root.path())).unwrap();

        assert_eq!(config.brain_path, root.path().join("data/latens"));
        assert_eq!(config.embedding_provider, EmbeddingProvider::Onnx);
        assert_eq!(config.embedding_model, None);
        assert_eq!(config.duplicates_threshold, None);
        assert_eq!(config.sync_timeout, Duration::from_secs(30));
    }

    fn write_config(root: &Path, text: &str) {
        let dir = root.join("config/latens");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), text).unwrap();
    }

    #[test]
    fn a_file_sets_the_keys_it_names() {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            r#"
            [brain]
            path = "/brains/mine"

            [embedding]
            provider = "hash"

            [lock]
            timeout = "200ms"

            [sync]
            timeout = "1m 30s"
            "#,
        );

        let config = Config::load(&dirs(root.path())).unwrap();

        assert_eq!(config.brain_path, Path::new("/brains/mine"));
        assert_eq!(config.embedding_provider, EmbeddingProvider::Hash);
        assert_eq!(config.lock_timeout, Some(Duration::from_millis(200)));
        assert_eq!(config.sync_timeout, Duration::from_secs(90));
    }

    #[test]
    fn an_unknown_key_fails_and_names_it() {
        let root = tempfile::tempdir().unwrap();
        write_config(root.path(), "[sync]\ntimeuot = \"10s\"\n");

        let error = Config::load(&dirs(root.path())).unwrap_err().to_string();

        assert!(error.contains("timeuot"), "{error}");
    }

    #[test]
    fn an_unknown_table_fails_and_names_it() {
        let root = tempfile::tempdir().unwrap();
        write_config(
            root.path(),
            "[remote]\nurl = \"git@example.com:me/brain\"\n",
        );

        let error = Config::load(&dirs(root.path())).unwrap_err().to_string();

        assert!(error.contains("remote"), "{error}");
    }

    #[test]
    fn a_relative_brain_path_fails() {
        let root = tempfile::tempdir().unwrap();
        write_config(root.path(), "[brain]\npath = \"brains/mine\"\n");

        let error = Config::load(&dirs(root.path())).unwrap_err().to_string();

        assert!(error.contains("brain.path"), "{error}");
        assert!(error.contains("absolute"), "{error}");
    }

    fn vars(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, value)| value.into())
        }
    }

    #[test]
    fn dirs_come_from_the_xdg_variables() {
        let dirs = Dirs::from_vars(vars(&[
            ("HOME", "/home/owner"),
            ("XDG_CONFIG_HOME", "/xdg/config"),
            ("XDG_DATA_HOME", "/xdg/data"),
        ]))
        .unwrap();

        assert_eq!(dirs.config_home, Path::new("/xdg/config"));
        assert_eq!(dirs.data_home, Path::new("/xdg/data"));
    }

    #[test]
    fn unset_empty_or_relative_xdg_variables_fall_back_to_home() {
        for xdg in [None, Some(""), Some("relative/dir")] {
            let mut pairs = vec![("HOME", "/home/owner")];
            if let Some(value) = xdg {
                pairs.push(("XDG_CONFIG_HOME", value));
                pairs.push(("XDG_DATA_HOME", value));
            }

            let dirs = Dirs::from_vars(vars(&pairs)).unwrap();

            assert_eq!(dirs.config_home, Path::new("/home/owner/.config"));
            assert_eq!(dirs.data_home, Path::new("/home/owner/.local/share"));
        }
    }

    #[test]
    fn no_home_and_no_xdg_variables_fails() {
        let error = Dirs::from_vars(vars(&[])).unwrap_err().to_string();

        assert!(error.contains("HOME"), "{error}");
    }
}
