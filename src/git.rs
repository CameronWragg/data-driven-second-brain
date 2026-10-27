//! Runs the system `git`, so the owner's own git config and credentials apply
//! (SPEC §10).

use std::io;
use std::path::Path;
use std::process::Command;

/// Why a `git` command failed.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("couldn't run git: {0}")]
    Spawn(#[source] io::Error),
    #[error("`git {subcommand}` failed: {stderr}")]
    Failed { subcommand: String, stderr: String },
}

/// Runs `git` with `args` in `dir`, returning its stdout.
pub fn run(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(GitError::Spawn)?;
    if !output.status.success() {
        return Err(GitError::Failed {
            subcommand: args.first().copied().unwrap_or_default().to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
