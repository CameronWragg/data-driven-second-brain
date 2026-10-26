//! `TestBrain`, the harness every integration test goes through (SPEC §16.3).

#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A fresh tempdir standing in for the owner's machine. `latens` and `git`
/// run with only the environment set here, so no test can reach the owner's
/// home, brain, git config or credentials, `gh` login or the network.
pub struct TestBrain {
    dir: tempfile::TempDir,
}

impl TestBrain {
    pub fn new() -> TestBrain {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("home")).unwrap();
        // PATH holds git and nothing else, so `gh` is absent.
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink(find_on_path("git"), bin.join("git")).unwrap();
        TestBrain { dir }
    }

    /// `$HOME`, which also holds every XDG directory.
    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn config_home(&self) -> PathBuf {
        self.home().join(".config")
    }

    pub fn data_home(&self) -> PathBuf {
        self.home().join(".local/share")
    }

    /// Where `init` puts the brain folder by default.
    pub fn default_brain(&self) -> PathBuf {
        self.data_home().join("latens")
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_home().join("latens/config.toml")
    }

    pub fn dirs(&self) -> latens::config::Dirs {
        latens::config::Dirs {
            config_home: self.config_home(),
            data_home: self.data_home(),
        }
    }

    /// A command for `program` in the isolated environment, run from `$HOME`.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let home = self.home();
        let mut command = Command::new(program);
        command
            .env_clear()
            .current_dir(&home)
            .env("PATH", self.dir.path().join("bin"))
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", self.config_home())
            .env("XDG_DATA_HOME", self.data_home())
            .env("XDG_STATE_HOME", home.join(".local/state"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Test Owner")
            .env("GIT_AUTHOR_EMAIL", "owner@example.com")
            .env("GIT_COMMITTER_NAME", "Test Owner")
            .env("GIT_COMMITTER_EMAIL", "owner@example.com");
        // Anything that tries the network meets a closed port.
        for proxy in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            command.env(proxy, "http://127.0.0.1:9");
        }
        command
    }

    /// Runs `latens` with `args`.
    pub fn latens(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_latens"))
            .args(args)
            .output()
            .expect("latens runs")
    }

    /// Runs `git` in `dir` and returns its stdout, failing the test if it fails.
    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let output = self
            .command("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            stderr(&output)
        );
        stdout(&output)
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Fails the test, showing stderr, unless `output` succeeded.
pub fn assert_success(output: &Output) {
    assert!(output.status.success(), "failed: {}", stderr(output));
}

fn find_on_path(program: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{program} is on PATH"))
}
