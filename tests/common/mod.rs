//! `TestBrain`, the harness every integration test goes through (SPEC §16.3).

#![allow(dead_code)]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rmcp::model::CallToolRequestParams;
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::{RoleClient, ServiceExt};

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

    /// Creates a brain with `latens init --no-remote` at the default path.
    pub fn init(&self) -> PathBuf {
        assert_success(&self.latens(&["init", "--no-remote"]));
        self.default_brain()
    }

    /// Places an Entity file in the brain directly, as a fixture: `path` is
    /// relative to `entities/`.
    pub fn place_entity(&self, path: &str, text: &str) {
        let file = self.default_brain().join("entities").join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }

    /// Starts `latens serve` and connects rmcp's client to it over stdio.
    pub async fn serve(&self) -> RunningService<RoleClient, ()> {
        self.serve_with(self.command(env!("CARGO_BIN_EXE_latens")))
            .await
    }

    /// Starts `latens serve` with `dir` as its working directory.
    pub async fn serve_in(&self, dir: &Path) -> RunningService<RoleClient, ()> {
        let mut command = self.command(env!("CARGO_BIN_EXE_latens"));
        command.current_dir(dir);
        self.serve_with(command).await
    }

    /// Starts `latens serve` from `$HOME` with `CLAUDE_PROJECT_DIR` set to `dir`.
    pub async fn serve_for_project(&self, dir: &Path) -> RunningService<RoleClient, ()> {
        let mut command = self.command(env!("CARGO_BIN_EXE_latens"));
        command.env("CLAUDE_PROJECT_DIR", dir);
        self.serve_with(command).await
    }

    async fn serve_with(&self, mut command: Command) -> RunningService<RoleClient, ()> {
        command.arg("serve");
        let transport = TokioChildProcess::new(tokio::process::Command::from(command))
            .expect("latens serve starts");
        ().serve(transport)
            .await
            .expect("the MCP handshake succeeds")
    }

    /// Makes a Checkout named `name` outside the brain, with one commit on
    /// `main` and `remote`, if any, as its `origin`.
    pub fn checkout(&self, name: &str, remote: Option<&str>) -> PathBuf {
        let dir = self.dir.path().join("work").join(name);
        fs::create_dir_all(&dir).unwrap();
        self.git(&dir, &["init", "--quiet", "--initial-branch", "main"]);
        fs::write(dir.join("README.md"), format!("# {name}\n")).unwrap();
        self.git(&dir, &["add", "README.md"]);
        self.git(&dir, &["commit", "--quiet", "--message", "Start"]);
        if let Some(remote) = remote {
            self.git(&dir, &["remote", "add", "origin", remote]);
        }
        dir
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

/// Calls `tool` with `arguments`, returning whether it was an error and its text.
pub async fn call(
    client: &RunningService<RoleClient, ()>,
    tool: &'static str,
    arguments: serde_json::Value,
) -> (bool, String) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("tool arguments are an object");
    };
    let result = client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .unwrap_or_else(|error| panic!("`{tool}` answers: {error}"));
    let text = result
        .content
        .iter()
        .filter_map(|block| block.as_text())
        .map(|text| text.text.as_str())
        .collect();
    (result.is_error == Some(true), text)
}

/// Snapshot settings that redact what changes from run to run: ULIDs,
/// versions, commit SHAs and timestamps.
pub fn redacted() -> insta::Settings {
    let mut settings = insta::Settings::clone_current();
    for (pattern, replacement) in [
        (r"\b[0-9A-HJKMNP-TV-Z]{26}\b", "[ULID]"),
        (r"\b[0-9a-f]{40}\b", "[SHA]"),
        (r"\b[0-9a-f]{16}\b", "[VERSION]"),
        (r"\b\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z", "[TIME]"),
    ] {
        settings.add_filter(pattern, replacement);
    }
    settings
}

/// A Dependency Entity file for `dep:npm:prisma`, placed with
/// `TestBrain::place_entity` at `PRISMA_PATH`.
pub const PRISMA: &str = "---
id: dep:npm:prisma
ecosystem: npm
name: prisma
docs: []
links: []
---

The Prisma ORM.
";
pub const PRISMA_PATH: &str = "dependencies/npm/prisma.md";

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
