//! Recollection files: Markdown with YAML front-matter (SPEC §4.4).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The category of a Recollection. Only a change to this code adds one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Decision,
    Convention,
    Caveat,
    Architecture,
    HowTo,
}

impl Kind {
    pub const ALL: [Kind; 5] = [
        Kind::Decision,
        Kind::Convention,
        Kind::Caveat,
        Kind::Architecture,
        Kind::HowTo,
    ];

    /// The level-2 headings a body of this Kind must have.
    pub fn required_sections(self) -> &'static [&'static str] {
        match self {
            Kind::Decision => &["Context", "Decision", "Why"],
            Kind::Caveat => &["Symptom", "Cause", "Avoidance"],
            Kind::HowTo => &["Steps"],
            Kind::Convention | Kind::Architecture => &[],
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Decision => "decision",
            Kind::Convention => "convention",
            Kind::Caveat => "caveat",
            Kind::Architecture => "architecture",
            Kind::HowTo => "how-to",
        }
    }
}

/// The type of a Link (SPEC §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum LinkType {
    About,
    RelatesTo,
    DependsOn,
    Calls,
}

impl LinkType {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkType::About => "about",
            LinkType::RelatesTo => "relates-to",
            LinkType::DependsOn => "depends-on",
            LinkType::Calls => "calls",
        }
    }
}

/// A Link, stored in the front-matter of the file it starts from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Link {
    #[serde(rename = "type")]
    pub link_type: LinkType,
    pub to: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Active,
    Abandoned,
}

/// A Recollection's front-matter, in the order its keys are written.
/// Parsing is strict: a key this binary doesn't know is an error (SPEC §17.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontMatter {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub summary: String,
    pub state: State,
    pub abandoned_at: Option<String>,
    /// `None` is the Repository's default branch.
    pub branch: Option<String>,
    #[serde(default)]
    pub applies_to: BTreeMap<String, String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub supersedes: Vec<String>,
    pub created: String,
    pub updated: String,
}

/// A whole Recollection file.
#[derive(Debug, Clone, PartialEq)]
pub struct Recollection {
    pub front_matter: FrontMatter,
    pub body: String,
}

/// Why a Recollection file couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("it doesn't start with a `---` front-matter block")]
    NoFrontMatter,
    #[error("its front-matter is invalid: {0}")]
    Yaml(#[from] serde_saphyr::Error),
}

impl Recollection {
    /// The file's text: front-matter, a blank line, then the body.
    pub fn render(&self) -> String {
        let yaml =
            serde_saphyr::to_string(&self.front_matter).expect("front-matter always serialises");
        let body = self.body.trim_end();
        format!("---\n{yaml}---\n\n{body}\n")
    }

    pub fn parse(text: &str) -> Result<Recollection, ParseError> {
        let (yaml, body) = split_front_matter(text).ok_or(ParseError::NoFrontMatter)?;
        Ok(Recollection {
            front_matter: serde_saphyr::from_str(yaml)?,
            body: body.to_owned(),
        })
    }
}

/// Splits an Item's file into its YAML front-matter and its body, or `None`
/// if it doesn't start with a `---` block.
pub fn split_front_matter(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let (yaml, body) = match rest.split_once("\n---\n") {
        Some((yaml, body)) => (yaml, body),
        None => (rest.strip_suffix("\n---")?, ""),
    };
    Some((yaml, body.trim_start_matches('\n')))
}

/// The concurrency `version` of a file: a hash of its content (SPEC §5.1).
pub fn version(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The names of a body's level-2 headings, outside fenced code blocks.
pub fn sections(body: &str) -> Vec<&str> {
    let mut fence: Option<&str> = None;
    let mut names = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
        } else if trimmed.starts_with("```") {
            fence = Some("```");
        } else if trimmed.starts_with("~~~") {
            fence = Some("~~~");
        } else if let Some(name) = line.strip_prefix("## ") {
            names.push(name.trim().trim_end_matches('#').trim());
        }
    }
    names
}

/// The sections a body of `kind` needs but lacks, compared ignoring case.
pub fn missing_sections(kind: Kind, body: &str) -> Vec<&'static str> {
    let present = sections(body);
    kind.required_sections()
        .iter()
        .copied()
        .filter(|required| {
            !present
                .iter()
                .any(|name| name.eq_ignore_ascii_case(required))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caveat() -> Recollection {
        Recollection {
            front_matter: FrontMatter {
                id: "01JB3K8V9Q0000000000000000".into(),
                kind: Kind::Caveat,
                title: "Prisma migrations fail silently without a shadow database".into(),
                summary: "`prisma migrate dev` exits 0 but applies nothing: SHADOW_DATABASE_URL is unset."
                    .into(),
                state: State::Active,
                abandoned_at: None,
                branch: None,
                applies_to: BTreeMap::from([("dep:npm:prisma".into(), "<5".into())]),
                tags: vec!["migrations".into()],
                links: vec![Link {
                    link_type: LinkType::About,
                    to: "dep:npm:prisma".into(),
                }],
                supersedes: vec![],
                created: "2026-10-23T10:00:00Z".into(),
                updated: "2026-10-23T10:00:00Z".into(),
            },
            body: "## Symptom\n\nNothing applies.\n\n## Cause\n\nNo shadow database.\n".into(),
        }
    }

    #[test]
    fn a_file_is_front_matter_then_body() {
        let text = caveat().render();

        assert_eq!(
            text,
            r#"---
id: 01JB3K8V9Q0000000000000000
kind: caveat
title: Prisma migrations fail silently without a shadow database
summary: "`prisma migrate dev` exits 0 but applies nothing: SHADOW_DATABASE_URL is unset."
state: active
abandoned_at: null
branch: null
applies_to:
  "dep:npm:prisma": <5
tags:
- migrations
links:
- type: about
  to: dep:npm:prisma
supersedes: []
created: "2026-10-23T10:00:00Z"
updated: "2026-10-23T10:00:00Z"
---

## Symptom

Nothing applies.

## Cause

No shadow database.
"#
        );
    }

    #[test]
    fn a_rendered_file_parses_back_to_itself() {
        let recollection = caveat();

        let parsed = Recollection::parse(&recollection.render()).unwrap();

        assert_eq!(parsed, recollection);
    }

    #[test]
    fn an_unknown_front_matter_key_fails_to_parse() {
        let text = caveat()
            .render()
            .replace("state: active", "state: active\nmood: calm");

        let error = Recollection::parse(&text).unwrap_err().to_string();

        assert!(error.contains("mood"), "{error}");
    }

    #[test]
    fn missing_sections_ignore_case_and_code_fences() {
        let body = "## symptom\n\n```md\n## Cause\n```\n\n## Avoidance ##\n";

        assert_eq!(missing_sections(Kind::Caveat, body), ["Cause"]);
        assert_eq!(missing_sections(Kind::Convention, body), [] as [&str; 0]);
    }

    #[test]
    fn a_version_changes_with_the_content() {
        assert_eq!(version("a"), version("a"));
        assert_ne!(version("a"), version("b"));
        assert_eq!(version("a").len(), 16);
    }
}
