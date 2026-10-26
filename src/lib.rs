//! Latens holds a Second Brain and serves it to Agents over MCP.

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
    Init,
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
        Command::Init => "init",
        Command::Serve => "serve",
        Command::Reindex => "reindex",
        Command::Eval => "eval",
        Command::Hook(Hook::Stop) => "hook stop",
    };
    eprintln!("latens {name}: not implemented yet");
    ExitCode::FAILURE
}
