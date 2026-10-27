//! The IDs of things in the Second Brain (SPEC §4.1), and the files they live in.

use std::fmt;
use std::path::{Path, PathBuf};

use ulid::Ulid;

/// The ID of a Recollection or an Entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemId {
    Recollection(Ulid),
    Entity(EntityId),
}

/// The ID of an Entity: `repo:<host>/<owner>/<name>` or `dep:<ecosystem>:<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityId {
    Repository { path: String },
    Dependency { ecosystem: String, name: String },
}

impl ItemId {
    /// Reads an ID in any of its forms, or `None` if it is none of them.
    pub fn parse(id: &str) -> Option<ItemId> {
        if let Some(entity) = EntityId::parse(id) {
            return Some(ItemId::Entity(entity));
        }
        Ulid::from_string(id).ok().map(ItemId::Recollection)
    }

    /// The name of its kind of thing, as a tag that delimits its stored text.
    pub fn tag(&self) -> &'static str {
        match self {
            ItemId::Recollection(_) => "recollection",
            ItemId::Entity(_) => "entity",
        }
    }

    /// The item's file, relative to the brain folder (SPEC §3).
    pub fn file(&self) -> PathBuf {
        match self {
            ItemId::Recollection(id) => recollection_file(*id),
            ItemId::Entity(id) => id.file(),
        }
    }

    /// The item whose file is `path`, relative to the brain folder, or `None`
    /// if no item lives there.
    pub fn from_file(path: &str) -> Option<ItemId> {
        let stem = path.strip_suffix(".md")?;
        let id = if let Some(ulid) = stem.strip_prefix("recollections/") {
            ulid.to_owned()
        } else if let Some(repository) = stem.strip_prefix("entities/repositories/") {
            format!("repo:{repository}")
        } else {
            let (ecosystem, name) = stem
                .strip_prefix("entities/dependencies/")?
                .split_once('/')?;
            format!("dep:{ecosystem}:{name}")
        };
        // Only the file the ID maps back to, so `recollections/01jb….md`
        // isn't taken for `recollections/01JB….md`.
        ItemId::parse(&id).filter(|parsed| parsed.file() == Path::new(path))
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemId::Recollection(id) => write!(f, "{id}"),
            ItemId::Entity(id) => write!(f, "{id}"),
        }
    }
}

/// A Recollection's file, relative to the brain folder.
pub fn recollection_file(id: Ulid) -> PathBuf {
    PathBuf::from(format!("recollections/{id}.md"))
}

/// Whether `segment` can be one part of an Entity's file path: nothing that
/// climbs out of its folder, hides itself or needs quoting.
fn safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && !segment.starts_with('.')
        && !segment
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "\\\"<>".contains(c))
}

impl EntityId {
    /// Reads an Entity ID, or `None` if `id` isn't one.
    pub fn parse(id: &str) -> Option<EntityId> {
        if let Some(path) = id.strip_prefix("repo:") {
            let segments: Vec<&str> = path.split('/').collect();
            return (segments.len() == 3 && segments.iter().all(|s| safe_segment(s))).then(|| {
                EntityId::Repository {
                    path: path.to_owned(),
                }
            });
        }
        let (ecosystem, name) = id.strip_prefix("dep:")?.split_once(':')?;
        // npm's scoped packages (`@scope/name`) put one `/` in the name.
        let valid = safe_segment(ecosystem)
            && !ecosystem.contains('/')
            && name.split('/').count() <= 2
            && name.split('/').all(safe_segment_or_scope);
        valid.then(|| EntityId::Dependency {
            ecosystem: ecosystem.to_owned(),
            name: name.to_owned(),
        })
    }

    /// The Entity's file, relative to the brain folder (SPEC §3).
    pub fn file(&self) -> PathBuf {
        match self {
            EntityId::Repository { path } => {
                PathBuf::from(format!("entities/repositories/{path}.md"))
            }
            EntityId::Dependency { ecosystem, name } => {
                PathBuf::from(format!("entities/dependencies/{ecosystem}/{name}.md"))
            }
        }
    }
}

fn safe_segment_or_scope(segment: &str) -> bool {
    safe_segment(segment.strip_prefix('@').unwrap_or(segment))
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntityId::Repository { path } => write!(f, "repo:{path}"),
            EntityId::Dependency { ecosystem, name } => write!(f, "dep:{ecosystem}:{name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_ids_map_to_their_files() {
        for (id, file) in [
            (
                "repo:github.com/cameronwragg/api",
                "entities/repositories/github.com/cameronwragg/api.md",
            ),
            ("dep:npm:prisma", "entities/dependencies/npm/prisma.md"),
            (
                "dep:npm:@prisma/client",
                "entities/dependencies/npm/@prisma/client.md",
            ),
        ] {
            let parsed = ItemId::parse(id).unwrap();

            assert_eq!(parsed.file(), PathBuf::from(file));
            assert_eq!(parsed.to_string(), id);
        }
    }

    #[test]
    fn a_file_gives_back_the_id_of_the_item_that_lives_there() {
        for id in [
            "repo:github.com/cameronwragg/api",
            "dep:npm:prisma",
            "dep:npm:@prisma/client",
            "01JB3K8V9Q0000000000000000",
        ] {
            let parsed = ItemId::parse(id).unwrap();

            let file = parsed.file();

            assert_eq!(ItemId::from_file(file.to_str().unwrap()), Some(parsed));
        }
    }

    #[test]
    fn files_where_no_item_lives_have_no_id() {
        for path in [
            "brain.toml",
            "recollections/notes.md",
            "recollections/01jb3k8v9q0000000000000000.md",
            "recollections/01JB3K8V9Q0000000000000000.txt",
            "entities/repositories/github.com/owner.md",
            "entities/dependencies/npm/a/b/c.md",
            "entities/dependencies/npm/.hidden.md",
            "eval/queries.yaml",
        ] {
            assert_eq!(ItemId::from_file(path), None, "{path}");
        }
    }

    #[test]
    fn a_recollection_id_is_a_ulid_in_any_case() {
        let parsed = ItemId::parse("01jb3k8v9q0000000000000000").unwrap();

        assert_eq!(
            parsed.file(),
            PathBuf::from("recollections/01JB3K8V9Q0000000000000000.md")
        );
    }

    #[test]
    fn ids_that_would_leave_their_folder_are_not_ids() {
        for id in [
            "repo:github.com/../escape",
            "repo:github.com/owner",
            "repo:github.com/owner/name/extra",
            "dep:npm:../../brain",
            "dep:npm:.hidden",
            "dep:npm:a/b/c",
            "dep:npm",
            "repo:github.com/owner/na me",
            "not an id",
        ] {
            assert_eq!(ItemId::parse(id), None, "{id}");
        }
    }
}
