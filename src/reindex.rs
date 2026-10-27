//! `latens reindex`: rebuilds the index from the files, in place (SPEC §6.1).

use crate::brain::Brain;
use crate::config::{Config, Dirs};
use crate::index::Index;
use crate::lock::{self, WriteLock};

/// Rebuilds the index under the write lock, inside its database file, so any
/// `serve` that has it open carries on.
pub fn run() -> anyhow::Result<()> {
    let config = Config::load(&Dirs::from_env()?)?;
    let brain = Brain::open(&config.brain_path)?;
    let lock = WriteLock::take(
        brain.root(),
        config.lock_timeout.unwrap_or(lock::DEFAULT_TIMEOUT),
    )?;
    let rebuilt = Index::open(&brain)?.rebuild(&brain, &lock)?;
    let files = match rebuilt.indexed.len() {
        1 => "1 file".to_owned(),
        count => format!("{count} files"),
    };
    println!("Rebuilt the index from {files}.");
    Ok(())
}
