//! The `get` tool's core: whole Recollections and Entities by ID (SPEC §14.4).

use std::io;
use std::path::PathBuf;

use crate::brain::Brain;
use crate::id::ItemId;
use crate::recollection;

/// A file found by its ID, exactly as it is on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    pub version: String,
    pub text: String,
}

/// What `get` found, in request order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub items: Vec<Item>,
    /// IDs with no Recollection or Entity, as they were given.
    pub not_found: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("couldn't read {}: {source}", path.display())]
pub struct GetError {
    path: PathBuf,
    source: io::Error,
}

/// Reads each of `ids`. A hand-edited file is returned as it is, even one
/// that no longer parses.
pub fn get(brain: &Brain, ids: &[String]) -> Result<Found, GetError> {
    let mut found = Found {
        items: Vec::new(),
        not_found: Vec::new(),
    };
    for requested in ids {
        let Some(id) = ItemId::parse(requested) else {
            found.not_found.push(requested.clone());
            continue;
        };
        let path = brain.root().join(id.file());
        match std::fs::read_to_string(&path) {
            Ok(text) => found.items.push(Item {
                id,
                version: recollection::version(&text),
                text,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                found.not_found.push(requested.clone())
            }
            Err(source) => return Err(GetError { path, source }),
        }
    }
    Ok(found)
}
