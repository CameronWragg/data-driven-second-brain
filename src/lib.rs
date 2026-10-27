//! Latens holds a Second Brain and serves it to Agents over MCP.

pub mod brain;
pub mod config;
pub mod get;
mod git;
pub mod id;
pub mod identifiers;
pub mod index;
mod init;
pub mod lock;
pub mod recollection;
mod reindex;
mod serve;
pub mod write;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Holds a Second Brain and serves it to Agents over MCP.
#[derive(Debug, Parser)]
#[command(name = "latens", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a Second Brain, or join one with --clone
    Init {
        /// Create the brain without a remote; it can be given one later
        #[arg(long)]
        no_remote: bool,
        /// Put the brain folder here instead of $XDG_DATA_HOME/latens
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Run the MCP server over stdio; this is what Agents launch
    Serve,
    /// Rebuild the index from files, in place
    Reindex,
    /// Run the retrieval test set
    Eval,
    /// Hooks run by the Claude Code plugin
    #[command(subcommand)]
    Hook(Hook),
}

#[derive(Debug, Subcommand)]
pub enum Hook {
    /// The Claude Code plugin's Stop hook
    Stop,
}

/// Runs a parsed command line, returning the process exit code.
pub fn run(cli: Cli) -> ExitCode {
    let name = match cli.command {
        Command::Init { no_remote, path } => {
            return report("init", init::run(no_remote, path));
        }
        Command::Serve => return report("serve", serve::run()),
        Command::Reindex => return report("reindex", reindex::run()),
        Command::Eval => "eval",
        Command::Hook(Hook::Stop) => "hook stop",
    };
    eprintln!("latens {name}: not implemented yet");
    ExitCode::FAILURE
}

/// Turns a command's result into its exit code, printing any error.
fn report(name: &str, result: anyhow::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("latens {name}: {error:#}");
            ExitCode::FAILURE
        }
    }
}
