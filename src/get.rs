//! The `get` tool's core: whole Recollections and Entities by ID (SPEC §14.4).

use std::io;
use std::path::PathBuf;

use crate::brain::Brain;
use crate::id::ItemId;
use crate::index::{Hit, Index};
use crate::recollection;

/// How many Recollections about an Entity `get` lists (SPEC §14.4).
const ABOUT_LIMIT: usize = 10;

/// A Recollection or Entity found by its ID, exactly as it is on disk. An
/// Entity's resolved `docs` (SPEC §14.4) wait for the scanner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    pub version: String,
    pub text: String,
    /// For an Entity, the Recollections about it; `None` for a Recollection,
    /// or if the index couldn't be read.
    pub about: Option<About>,
}

/// The newest active Recollections about an Entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct About {
    /// At most 10.
    pub recollections: Vec<Hit>,
    /// How many there are in all.
    pub total: usize,
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

/// Reads each of `ids`, listing what `index`, if there is one, holds about
/// each Entity. A hand-edited file is returned as it is, even one that no
/// longer parses.
pub fn get(brain: &Brain, index: Option<&Index>, ids: &[String]) -> Result<Found, GetError> {
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
            Ok(text) => {
                let about = match (&id, index) {
                    (ItemId::Entity(entity), Some(index)) => index
                        .about(&entity.to_string(), ABOUT_LIMIT)
                        .ok()
                        .map(|(recollections, total)| About {
                            recollections,
                            total,
                        }),
                    _ => None,
                };
                found.items.push(Item {
                    version: recollection::version(&text),
                    id,
                    text,
                    about,
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                found.not_found.push(requested.clone())
            }
            Err(source) => return Err(GetError { path, source }),
        }
    }
    Ok(found)
}
