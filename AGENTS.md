# Latens

Latens holds a Second Brain and serves it to Agents over MCP. It is one Rust binary, `latens`.

## Before you change anything

Read `GLOSSARY.md`, `docs/SPEC.md` and the ADRs in `docs/adr/`. Name things with the glossary's terms in code, tests and prose. When work contradicts the SPEC or an ADR, raise it on the issue before writing code.

## Where you work

Every build, test and agent session runs inside the dev container (ADR-0006). From the host:

- `GH_TOKEN=$(gh auth token) devcontainer up --workspace-folder .`
- `devcontainer exec --workspace-folder . just <recipe>`

`just --list` shows every recipe. The ones that matter:

- `just check`: the full local gate. It passes before every PR.
- `just test`: the tests alone; arguments go to nextest.
- `just dev-serve`, `just dev-claude`, `just inspect`: run the binary against the dev brain in `.dev/`, never the owner's real brain. Run `latens` only through these.
- `just dev-reset`: wipe the dev brain.

Docs name recipes and never copy their commands; CI fails a PR whose `AGENTS.md` or README names a recipe that doesn't exist (`scripts/check-recipes.sh`).

CI runs natively on Linux and macOS, not in the container. A separate workflow runs `just check` in the dev container, only when the environment changes. Workflows use only first-party actions, pinned by full commit SHA (ADR-0007).

The versions of `just`, `cargo-nextest` and `cargo-deny` live only in `.config/tool-versions.toml`; the Rust version lives only in `rust-toolchain.toml`. A pin bump is its own PR.

## Pull requests

- One issue per PR, with `Closes #<n>` in the body.
- Changing the dev environment (container, recipes, tool versions) means updating this file in the same PR.

## Issues

Labels drive the backlog, grouped by milestone:

- `ready-for-agent`: fully specified, so pick it up.
- `ready-for-human`: the owner must do or decide it, so leave it.
- `needs-triage`, `needs-info`: not ready, so leave it.
