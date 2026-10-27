//! Entity files: Markdown with YAML front-matter whose body is a free-form
//! description (SPEC §4.2).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::EntityId;
use crate::recollection::{self, LinkType, ParseError};

/// A whole Entity file.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub fields: Fields,
    /// The description, as Markdown.
    pub description: String,
}

/// An Entity's front-matter, which depends on what it is.
#[derive(Debug, Clone, PartialEq)]
pub enum Fields {
    Repository(Repository),
    Dependency(Dependency),
}

/// A Repository's front-matter, in the order its keys are written. Parsing
/// is strict: a key this binary doesn't know is an error (SPEC §17.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub id: String,
    pub name: String,
    /// `None` when the Repository isn't checked out here, so it isn't known.
    pub default_branch: Option<String>,
    /// The Repository this one is a fork of.
    #[serde(default)]
    pub forked_from: Option<String>,
    #[serde(default)]
    pub components: Vec<Component>,
    #[serde(default)]
    pub branches: BTreeMap<String, BranchState>,
    #[serde(default)]
    pub links: Vec<EntityLink>,
    #[serde(default)]
    pub manifest_hashes: BTreeMap<String, String>,
}

/// A Dependency's front-matter, in the order its keys are written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub id: String,
    pub ecosystem: String,
    pub name: String,
    #[serde(default)]
    pub docs: Vec<Doc>,
    #[serde(default)]
    pub links: Vec<EntityLink>,
}

/// A member of a monorepo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    pub path: String,
    pub name: String,
}

/// What is known of a Branch with Recollections on it (SPEC §9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchState {
    pub status: BranchStatus,
    /// The Branch it was merged into.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion: Option<PromotionStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BranchStatus {
    Open,
    Merged,
    Closed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromotionStatus {
    Pending,
}

/// Where a Dependency's documentation is for a range of its versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Doc {
    /// The versions it covers, as the ecosystem writes a range (`>=5`, `4.x`).
    pub versions: String,
    pub url: String,
}

/// A Link from an Entity (SPEC §4.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityLink {
    #[serde(rename = "type")]
    pub link_type: LinkType,
    pub to: String,
    /// The version a `depends-on` Link carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Who derived the Link, if not an Agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<LinkSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkSource {
    Scanner,
}

impl Entity {
    /// A new Entity with ID `id`, named `name`, and nothing else known.
    pub fn new(id: &EntityId, name: &str, description: &str) -> Entity {
        let fields = match id {
            EntityId::Repository { .. } => Fields::Repository(Repository {
                id: id.to_string(),
                name: name.to_owned(),
                default_branch: None,
                forked_from: None,
                components: Vec::new(),
                branches: BTreeMap::new(),
                links: Vec::new(),
                manifest_hashes: BTreeMap::new(),
            }),
            EntityId::Dependency { ecosystem, .. } => Fields::Dependency(Dependency {
                id: id.to_string(),
                ecosystem: ecosystem.clone(),
                name: name.to_owned(),
                docs: Vec::new(),
                links: Vec::new(),
            }),
        };
        Entity {
            fields,
            description: description.to_owned(),
        }
    }

    /// Reads the file of Entity `id`.
    pub fn parse(id: &EntityId, text: &str) -> Result<Entity, ParseError> {
        let (yaml, body) =
            recollection::split_front_matter(text).ok_or(ParseError::NoFrontMatter)?;
        let fields = match id {
            EntityId::Repository { .. } => Fields::Repository(serde_saphyr::from_str(yaml)?),
            EntityId::Dependency { .. } => Fields::Dependency(serde_saphyr::from_str(yaml)?),
        };
        Ok(Entity {
            fields,
            description: body.to_owned(),
        })
    }

    /// The file's text: front-matter, then the description after a blank
    /// line if there is one.
    pub fn render(&self) -> String {
        let yaml = match &self.fields {
            Fields::Repository(fields) => serde_saphyr::to_string(fields),
            Fields::Dependency(fields) => serde_saphyr::to_string(fields),
        }
        .expect("front-matter always serialises");
        match self.description.trim() {
            "" => format!("---\n{yaml}---\n"),
            description => format!("---\n{yaml}---\n\n{description}\n"),
        }
    }

    pub fn name(&self) -> &str {
        match &self.fields {
            Fields::Repository(fields) => &fields.name,
            Fields::Dependency(fields) => &fields.name,
        }
    }

    pub fn set_name(&mut self, name: String) {
        match &mut self.fields {
            Fields::Repository(fields) => fields.name = name,
            Fields::Dependency(fields) => fields.name = name,
        }
    }

    pub fn links(&self) -> &[EntityLink] {
        match &self.fields {
            Fields::Repository(fields) => &fields.links,
            Fields::Dependency(fields) => &fields.links,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPOSITORY: &str = r#"---
id: repo:github.com/cameronwragg/api
name: api
default_branch: main
forked_from: null
components:
- path: packages/core
  name: core
branches:
  feature/x:
    status: open
  feature/y:
    status: merged
    into: develop
    promotion: pending
links:
- type: depends-on
  to: dep:npm:prisma
  version: "5.14.0"
  by: scanner
manifest_hashes:
  package.json: abc
---

The API server.
"#;

    #[test]
    fn a_repository_file_parses_and_renders_back_to_the_same_text() {
        let id = EntityId::parse("repo:github.com/cameronwragg/api").unwrap();

        let entity = Entity::parse(&id, REPOSITORY).unwrap();

        assert_eq!(entity.name(), "api");
        assert_eq!(entity.description, "The API server.\n");
        assert_eq!(entity.render(), REPOSITORY);
    }

    #[test]
    fn a_dependency_file_parses_and_renders_back_to_the_same_text() {
        let id = EntityId::parse("dep:npm:prisma").unwrap();
        let text = "---\nid: dep:npm:prisma\necosystem: npm\nname: prisma\ndocs:\n- versions: '>=5'\n  url: https://www.prisma.io/docs\nlinks: []\n---\n\nThe Prisma ORM.\n";

        let entity = Entity::parse(&id, text).unwrap();

        assert_eq!(Entity::parse(&id, &entity.render()).unwrap(), entity);
        let Fields::Dependency(fields) = &entity.fields else {
            panic!("a dependency");
        };
        assert_eq!(fields.docs[0].versions, ">=5");
    }

    #[test]
    fn an_unknown_front_matter_key_fails_to_parse() {
        let id = EntityId::parse("repo:github.com/cameronwragg/api").unwrap();

        let error = Entity::parse(
            &id,
            &REPOSITORY.replace("name: api", "name: api\nmood: calm"),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("mood"), "{error}");
    }
}
