# Second Brain — v1 Specification

The Second Brain is a local, AI-first knowledge system for software engineering work. Agents use it through MCP to record and retrieve distilled knowledge about Repositories and Dependencies. Terms in **bold** are defined in [GLOSSARY.md](../GLOSSARY.md); architectural decisions live in [docs/adr/](./adr/).

## 1. Scope

**In v1:** a single owner; a local MCP server over stdio; a built-in manifest scanner; hybrid search; Branch-aware knowledge with Promotion; git-backed storage pushed to a private remote.

**Not in v1:** multiple users and hosting (the core stays independent of the transport so HTTP can be added later); public Source plugins (the scanner uses the Source interface internally); mirroring code; staleness checks that parse code; background processes of any kind ([ADR-0005](./adr/0005-no-background-processes.md)).

## 2. Process model

There is one binary (provisional name `brain`) with these subcommands:

| Command | Purpose |
|---|---|
| `brain init` | Create a Second Brain (§12) |
| `brain serve` | Run the MCP server over stdio; this is what Agents launch |
| `brain reindex` | Delete and rebuild the index from files |
| `brain eval` | Run the retrieval test set (§13) |

`serve` does work only while an Agent is connected.

**Startup sequence (`serve`):**
1. Open the index (SQLite, WAL mode).
2. Take the write lock (§11), then:
   1. Commit any uncommitted changes in the brain folder with an `Author: owner` trailer (§10).
   2. Pull from the remote, rebasing local commits, and re-index the files changed by the pull.
   3. Re-index any file whose content hash differs from the index, which picks up hand edits.
   4. Delete Recollections that have been `abandoned` for more than 14 days.
   5. Push any unpushed commits.
3. Detect the **Checkout** from the working directory, which gives the Repository and Branch.
4. If any manifest hash differs from the last scan, run the scanner (§7).
5. Detect the Branch lifecycle for the current Repository (§9.1). This needs the network and is skipped quietly when offline.

All network steps are best-effort: when offline, the brain carries on with local data and reports this through `context`.

**Shutdown:** push any unpushed commits.

## 3. Brain folder

```
<brain>/                                        # its own git repo, pushed to a private remote (ADR-0004)
  recollections/<ulid>.md
  entities/repositories/<host>/<owner>/<name>.md
  entities/dependencies/<ecosystem>/<name>.md
  eval/queries.yaml                             # retrieval test set (§13)
  .index/                                       # git-ignored: SQLite index and lock file
```

By default the folder is at `$XDG_DATA_HOME/second-brain`. Files are the source of truth, and `.index/` can be rebuilt from them at any time ([ADR-0001](./adr/0001-files-as-truth-sqlite-as-index.md)).

## 4. Data model

### 4.1 Identifiers

| Thing | ID form | Example |
|---|---|---|
| Recollection | ULID | `01JB3K…` |
| Repository | `repo:<host>/<owner>/<name>` | `repo:github.com/cameronwragg/api` |
| Dependency | `dep:<ecosystem>:<name>` | `dep:npm:prisma` |
| Branch | `<repository id>#<branch>` | `repo:github.com/cameronwragg/api#feature/x` |

**Repository identity** is the normalised remote URL of `origin`:
- lower-case the host
- remove `.git` from the end
- convert SSH and HTTPS forms to the same `host/owner/name` form

A Repository with no remote gets `repo:local/<ulid>`, which is recorded in the Checkout (§4.3).

### 4.2 Entity files

Every Entity file is Markdown with YAML front-matter. The body is a free-form description, which is embedded.

**Repository**
```yaml
id: repo:github.com/cameronwragg/api
name: api
default_branch: main
forked_from: null                  # Repository id, if a fork
components:                        # monorepo members (name provisional)
  - { path: packages/core, name: core }
branches:
  feature/x: { status: open }      # open | merged | closed | unknown
  feature/y: { status: merged, into: develop, promotion: pending }
links: [ { type: depends-on, to: dep:npm:prisma, version: "5.14.0" } ]
manifest_hashes: { package.json: "…" }
```

**Dependency**
```yaml
id: dep:npm:prisma
ecosystem: npm
name: prisma
docs:                              # matched against the installed version (§7.3)
  - { versions: ">=5", url: "https://www.prisma.io/docs" }
  - { versions: "4.x", url: "https://www.prisma.io/docs/v4" }
links: []
```

### 4.3 Checkouts

Checkouts are specific to one machine and are never committed. The index records each Checkout's path and the Repository it belongs to. A Repository with no remote is recognised again through a `.git/second-brain-id` file inside its Checkout.

### 4.4 Recollections

A Recollection is a Markdown file with YAML front-matter, stored at `recollections/<ulid>.md`.

```yaml
id: 01JB3K…
kind: caveat                       # decision | convention | caveat | architecture | how-to
title: Prisma migrations fail silently without a shadow database
summary: `prisma migrate dev` exits 0 but applies nothing when SHADOW_DATABASE_URL is unset.
state: active                      # active | abandoned
abandoned_at: null
branch: null                       # null = the Repository's default branch
applies_to: { "dep:npm:prisma": "<5" }   # optional version ranges
tags: [migrations]
links:
  - { type: about, to: dep:npm:prisma }
  - { type: about, to: repo:github.com/cameronwragg/api }
anchors:
  - repository: repo:github.com/cameronwragg/api
    path: prisma/schema.prisma
    lines: [12, 30]
    commit: 3f2a9c1
    hash: "sha256:…"               # hash of the normalised text of those lines at that commit
supersedes: [01HZX…]               # IDs this replaced; they now live only in git history
created: 2026-10-23T10:00:00Z
updated: 2026-10-23T10:00:00Z
```

`title` and `summary` are required. `summary` is one line.

**Required body sections by Kind** (level-2 headings, checked by name):

| Kind | Required | Optional |
|---|---|---|
| `decision` | Context, Decision, Why | Alternatives |
| `caveat` | Symptom, Cause, Avoidance | |
| `how-to` | Steps | |
| `convention` | — | |
| `architecture` | — | |

Kinds can be added only by a deliberate change to the code. Agents can't invent them.

### 4.5 Links

A Link is stored in the front-matter of the file it starts from. Initial Link types:

| Type | From → To |
|---|---|
| `about` | Recollection → Entity |
| `relates-to` | Recollection → Recollection |
| `depends-on` | Repository → Repository or Dependency (carries `version`) |
| `calls` | Repository → Repository |

Forks (`forked_from`), Components and Branches are structural fields, not Links. Links are unweighted facts. **Similarity** is computed at query time and never stored as a Link.

## 5. Writing

All changes go through the `write` tool as a **batch**. A batch is atomic: every operation is validated first, and if any operation fails, nothing is written.

**Operations:** `create`, `update`, `supersede` (create a replacement, record `supersedes`, delete the old Recollection), `delete`, `link`, `unlink`, `update_entity`.

**Checks, in order:**
1. **Schema:** required fields are present and the Kind's required sections exist.
2. **Secrets:** a pattern- and entropy-based detector (API keys, tokens, private keys, credentials in URLs). A match rejects the whole batch with an explanation.
3. **Duplicates** (on `create` only): the brain looks for existing Recollections that share an Entity or Anchor *and* have Similarity above a threshold. If any match, the batch is rejected and the candidates returned, unless the operation sets `distinct: true`.

**Commit:** each successful batch is one commit, made under the write lock. The subject is generated, and the trailers look like:
```
Recollection: 01JB3K…
Recollection: 01JB3M…
Repository: repo:github.com/cameronwragg/api
Branch: feature/x
Action: create, supersede
Author: agent
```

Pushing is separate and explicit (§10).

## 6. Index and search

### 6.1 Index

The index is one SQLite database in `.index/`, running in WAL mode. It holds:
- a table of files with their content hashes
- an FTS5 table
- a `sqlite-vec` table for embeddings
- a Links table
- Checkouts
- metadata, including the embedding model ID and its dimensions

If the configured model differs from the one recorded, the brain rebuilds every embedding.

### 6.2 Preprocessing before embedding

1. **Context prefix:** Kind, the names of linked Entities, and the Repository, e.g. `caveat · Dependency: npm:prisma · Repository: api ·`.
2. **Text:** title + summary + body. Markdown formatting is stripped and whitespace collapsed, but identifiers and paths are kept.
3. **Model labels:** the model's document label (`search_document:` / `passage:`) on stored text, and its query label on queries.
4. **Hash check:** the hash of the final text is stored. If it hasn't changed, the brain skips embedding.

Each Entity gets one embedding, built from its name and description. Each Recollection gets one embedding, with no chunking.

For keyword search, the FTS5 table indexes both the original text and a version with identifiers split (`getUserById` → `get user by id`).

### 6.3 Embedding provider

The provider is a trait. The default implementation is a local ONNX model, chosen using the test set (§13); candidates include bge-small-en-v1.5, nomic-embed-text-v1.5 and EmbeddingGemma. An API-backed provider can be swapped in later.

### 6.4 Search

1. **Scope.** By default the search covers:
   - the current Repository
   - Entities Linked to it **directly** (one step away), and Recollections about them
   - on Branches, the current Branch plus the default branch

   Recollections from other Branches are included only if the caller asks, and are labelled with their Branch. `scope: all` searches the whole brain.
2. **Filters:** Kind, Entity, tags, and whether to include abandoned Recollections.
3. **Ranking:** FTS5 and vector results are merged with reciprocal rank fusion.
4. **Results:** `id`, `kind`, `title`, `summary`, `branch`, `stale` (`fresh` | `stale` | `unknown`), and `score`. The full body comes from `get`.

## 7. Scanner (the first Source)

### 7.1 When it runs

The scanner runs at startup when a manifest hash has changed, or when an Agent calls the `scan` tool. It only reads the current Checkout.

### 7.2 Manifests (v1)

| Ecosystem | Manifests (direct dependencies) | Lockfiles (installed version) |
|---|---|---|
| `npm` | `package.json` | `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock` |
| `crates` | `Cargo.toml` | `Cargo.lock` |
| `pypi` | `pyproject.toml` (PEP 621, Poetry), `requirements*.txt` | `uv.lock`, `poetry.lock` |
| `go` | `go.mod` | — (`go.mod` already pins versions) |

The scanner reads only **direct** dependencies; indirect ones are ignored. Workspace or monorepo members become Components.

### 7.3 Versions and documentation locations

Version ranges follow each ecosystem's own rules: semver for npm, crates and go, and PEP 440 for pypi. A version that can't be parsed, such as a commit hash, matches the docs entry for the latest version.

The scanner fills in the first `docs` entry:
- docs.rs for crates
- pkg.go.dev for go
- registry metadata for npm and pypi

Agents can add and fix entries.

### 7.4 Repository-to-Repository Links

The scanner adds `depends-on` Links where a manifest points at a Repository the brain already knows about: a git dependency, a path dependency or a workspace member. Agents declare the rest, such as `calls`.

## 8. Anchors and staleness

When an Anchor is written, the brain stores its path, line range, commit and the hash of the normalised text in those lines.

On read, each Anchor is checked:
1. **No local Checkout** of the Repository → `unknown`.
2. **The file hasn't changed** since the Anchor's commit → `fresh`.
3. **Otherwise**, the brain searches the current file for the anchored text (normalised). If found, it's `fresh`, and the result includes the new line range so the Agent can update the Anchor. If not found, it's `stale`.

A Recollection is `stale` if any of its Anchors is stale. The check never parses code and never deletes anything automatically.

## 9. Branches and Promotion

### 9.1 Detecting what happened to a Branch

The brain only checks Branches that have Recollections on them.

1. `git ls-remote --heads origin`: a Branch that's no longer on the remote is a candidate.
2. If `gh` is installed and logged in, look up the PR for that branch:
   - **merged** → status `merged`, `into` = the PR's base branch
   - **closed** → status `closed`
3. Otherwise, check whether the Branch's last known commit is an ancestor of the default branch. If it is, the Branch was merged into the default branch.
4. If none of these settles it, the status is `unknown`, and `context` reports it so the Agent can resolve it with `status`.

The `status` tool sets the outcome by hand: `merged` (with `into`, which defaults to the default branch) or `closed`.

### 9.2 Outcomes

- **closed:** every Recollection on the Branch becomes `abandoned`, with `abandoned_at` set, and is deleted at startup after 14 days.
- **merged:** a pending Promotion is created for the Branch, and `context` announces it to the next Agent connected in that Repository.

### 9.3 Promotion

Calling `promotion` returns a **plan** for one pending Promotion:
- **The Branch's Recollections**, each with overlapping Recollections already on the target branch. Overlap means a shared Entity or Anchor, or high Similarity.
- **The target branch's Recollections whose anchored code changed in the merge**, with their staleness.
- **Anchors re-checked against the merge commit.**

The Agent resolves each item using `write`:

| Outcome | Operation |
|---|---|
| Add | `update`: set `branch` to the target, re-anchor to the merge commit |
| Fold in | `update` the existing Recollection, `delete` the branch one |
| Supersede | `supersede` the existing Recollection with the branch one, re-targeted |
| Discard | `delete` |

The Promotion is complete once no `active` Recollections remain on the merged Branch. Its status is then cleared, and the Branch is removed from the Repository's `branches` list. Merges that pass through intermediate branches (`feature/x` → `develop` → `main`) promote one step at a time.

## 10. Git, sync and hand edits

- The brain uses the system `git` command, so the owner's existing git credentials apply.
- Each batch is one commit (§5). Pushes happen on `push`, at shutdown, and at startup (to catch a missed shutdown push). There is no timer.
- Before pushing, the brain pulls with `--rebase`. Conflicts are rare because each Recollection has its own file. If a rebase does conflict, the brain aborts it and reports the conflict through `context`.
- **Hand edits:** uncommitted changes found at startup are committed with `Author: owner`. A file that fails the schema checks (§5) is still indexed as-is, and `context` lists it as needing repair.

## 11. Concurrency

Several `serve` processes can share one brain. `.index/lock` is an exclusive file lock. It's held for the startup maintenance steps and for each "write files → update index → commit" sequence, and other writers wait for it with a timeout. Reads never take the lock; WAL mode keeps them consistent.

## 12. `brain init`

1. Create the brain folder (default `$XDG_DATA_HOME/second-brain`) and its layout, with `.index/` git-ignored.
2. Run `git init` and make the first commit.
3. Create a **private** remote with `gh repo create --private` if `gh` is available; otherwise ask for a remote URL.
4. Print a generic MCP server config (`command`, `args`, stdio transport) plus short examples for common clients.

## 13. Retrieval test set

`eval/queries.yaml` lists queries, each with the Recollection IDs it should find and an optional scope. `brain eval` reports recall@5, recall@10 and mean reciprocal rank (MRR). It's used to choose the embedding model, check that preprocessing helps, and catch search getting worse. This repo also includes a small fixture brain with its own test set for CI.

## 14. MCP tools

| Tool | Input (summary) | Output (summary) |
|---|---|---|
| `context` | — | Current Repository, Branch and Checkout; directly Linked Entities; pending Promotions; Branches with status `unknown`; count of stale Recollections; files needing repair; offline or sync problems |
| `search` | `query`, `scope?`, `kinds?`, `entities?`, `tags?`, `branches?`, `include_abandoned?`, `limit?` | Summaries (§6.4) |
| `get` | `ids[]` | Full Recollections or Entities, with per-Anchor staleness and docs URLs resolved for the installed version |
| `write` | `operations[]` | Commit ID, or a rejection (schema, secret or duplicate, with candidates) |
| `promotion` | `branch?` | A Promotion plan (§9.3) |
| `status` | `branch`, `outcome: merged \| closed`, `into?` | Updated Branch status |
| `scan` | — | Entities and Links added, changed or removed |
| `history` | `id` | Past versions from git, found by `Recollection:` trailer, including superseded and deleted ones |
| `push` | — | Push result |
