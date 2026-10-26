use std::process::ExitCode;

use clap::Parser;
use latens::Cli;

fn main() -> ExitCode {
    latens::run(Cli::parse())
}
