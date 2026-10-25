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

`serve` does work only while an Agent is connected. It answers the MCP handshake (`initialize`, `tools/list`) at once and runs the startup sequence alongside it: Claude Code waits at most 5 s for the tools of a server that marks tools `anthropic/alwaysLoad` (§14.3). A tool call that arrives before the startup sequence finishes waits for it, up to a timeout, then returns `busy`.

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

A Link derived by the scanner carries `by: scanner`. The scanner adds and removes only its own Links, and an Agent cannot `unlink` one (it changes the manifest instead). Agents may declare Links of any type.

Forks (`forked_from`), Components and Branches are structural fields, not Links. Links are unweighted facts. **Similarity** is computed at query time and never stored as a Link.

## 5. Writing

All changes go through the `write` tool as a **batch** of operations. A batch is atomic: every operation is validated first, and if any operation fails, nothing is written.

### 5.1 Operations

| Operation | Fields | Notes |
|---|---|---|
| `create` | `kind`, `title`, `summary`, `body`, `links`; optional `tags`, `anchors`, `applies_to`, `ref`, `distinct` | `links` must hold at least one `about` Link |
| `update` | `id`, `version`; any of `title`, `summary`, `body`, `kind`, `tags`, `links`, `anchors`, `applies_to` | Patches the fields given. `body` and lists are replaced whole |
| `supersede` | `id`, `version`, `replacement` (the `create` fields) | Creates the replacement, records `supersedes`, deletes the old Recollection |
| `delete` | `id`, `version` | |
| `link` / `unlink` | `from`, `type`, `to`, `from_version`; optional `version` (the `depends-on` version) | `from_version` is the concurrency version of the file the Link starts from |
| `create_entity` | `id`, `name`, optional `description`, `docs`, `ref` | For Entities the scanner doesn't make: a `calls` target not checked out here, a Dependency outside §7.2 |
| `update_entity` | `id`, `version`; any of `name`, `description`, `docs` | All other Entity fields belong to the server and scanner |
| `set_branch_status` | `branch`, `outcome: merged \| closed`, optional `into` (defaults to the default branch) | Settles a Branch by hand (§9.1) |

**Fields the server fills in.** Agents never send `id` (on `create`), `created`, `updated`, `state`, `abandoned_at`, `branch`, or an Anchor's `commit` and `hash`. `branch` is the current Branch of the detected Checkout; only an `update` made for a Promotion (§9.3) may re-target it. An Anchor takes `path`, `lines` and an optional `repository` (default: the current Repository); the server computes `commit` and `hash` from the Checkout. If the anchored lines have uncommitted changes, the Anchor records the working-tree text, the commit is HEAD, and it is marked `uncommitted: true` (§8).

**References within a batch.** A `create` or `create_entity` may carry a local `ref` (e.g. `"a"`), and later operations in the same batch may name it as `ref:a`. The result maps each ref to its new ID.

**Unknown IDs.** A `link` to an ID that doesn't exist is rejected; Entities are created explicitly with `create_entity`.

**Optimistic concurrency.** Every Recollection and Entity has a `version`: a hash of its file's content, returned by `get`, `search`, `promotion` and `write`. Operations that change an existing file require the `version` the Agent last saw. If the file has changed since (a hand edit, another `serve`), the operation is rejected with code `conflict` and the current version.

### 5.2 Checks

Every check runs on every operation, and a rejection reports **all** problems at once, each with its operation index and check.

1. **Schema:** required fields are present, the Kind's required sections exist, a `create` has an `about` Link, and every referenced ID exists.
2. **Secrets:** a pattern- and entropy-based detector (API keys, tokens, private keys, credentials in URLs).
3. **Concurrency:** each `version` matches the file on disk.
4. **Duplicates** (on `create` and on a `supersede` replacement, for operations that passed the schema check): the brain looks for existing Recollections that share an Entity or Anchor *and* have Similarity above a threshold. It leaves out the Recollection being superseded and any the new one already Links with `relates-to`; such a Link counts as distinct.

**A duplicate rejection** lists at most 3 candidates, ranked, without scores, each with its `id`, `kind`, `title`, `summary` and `version`, followed by the decision:

| If the candidate is… | Then |
|---|---|
| the same knowledge | `update` or `supersede` it instead |
| related but distinct | add a `relates-to` Link to it and retry |
| unrelated | retry with `distinct` set to a short reason |

`distinct` is a reason string, not a flag, and it is recorded in the commit as a `Distinct:` trailer so it can be audited.

### 5.3 Commit

Each successful batch is one commit, made under the write lock. The subject is generated, and the trailers look like:
```
Recollection: 01JB3K…
Recollection: 01JB3M…
Repository: repo:github.com/cameronwragg/api
Branch: feature/x
Action: create, supersede
Distinct: covers the v5 behaviour; 01HZX… covers v4
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

   Recollections from other Branches are included only if the caller asks (`branches`: a list of names, or `"all"`), and are labelled with their Branch. `scope` is `"current"` (the default), `"all"`, or a Repository ID. With no detected Checkout, `"current"` behaves as `"all"`, and the results say so.
2. **Filters:** Kind, Entity, tags, and whether to include abandoned Recollections.
3. **Ranking:** FTS5 and vector results are merged with reciprocal rank fusion.
4. **Results:** Recollections and Entities, ranked, each with `id`, `kind` (`repository` or `dependency` for Entities), `title`, `summary`, `version`, and `branch` and staleness only when they apply. Scores are not returned; the rank carries the order. The full body comes from `get`.

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

The scanner adds `depends-on` Links (marked `by: scanner`, §4.5) where a manifest points at a Repository the brain already knows about: a git dependency, a path dependency or a workspace member. Agents declare the rest, such as `calls`.

## 8. Anchors and staleness

When an Anchor is written, the brain stores its path, line range, commit and the hash of the normalised text in those lines. If those lines have uncommitted changes, it hashes the working-tree text, records HEAD as the commit and marks the Anchor `uncommitted: true`; on read, such an Anchor skips step 2 and goes straight to the text search in step 3.

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
4. If none of these settles it, the status is `unknown`, and `context` reports it so the Agent can resolve it with a `set_branch_status` operation in `write` (§5.1): `merged` (with `into`, which defaults to the default branch) or `closed`.

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

### 14.1 Tools

| Tool | Input | Output |
|---|---|---|
| `context` | `path?` (re-detect the Checkout from this directory) | Current Repository, Branch and Checkout (or that there is none); directly Linked Entities; pending Promotions; Branches with status `unknown`; count of stale Recollections; files needing repair; offline or sync problems; then a one-line list of what needs doing |
| `search` | `query`, `scope?`, `branches?`, `kinds?`, `entities?`, `tags?`, `include_abandoned?`, `limit?` | Ranked results (§6.4), one line each |
| `get` | `ids[]` (Recollections or Entities) | Full items (§14.4) |
| `write` | `operations[]` (§5.1) | Per-operation outcomes and the commit, or a rejection (§5.2) |
| `promotion` | `branch?` (default: the oldest pending Promotion in the current Repository) | A Promotion plan (§9.3) as summaries |
| `scan` | — | Counts of Entities and Links added, changed and removed, then up to 20 named changes and "+N more" |
| `history` | `id`, `commit?` | Without `commit`: up to 20 commits for the item, newest first (short SHA, date, `Action`, `Author`, subject), found by trailer, including superseded and deleted items. With `commit`: the file as it was at that commit |
| `push` | — | Push result |

Tool names carry no prefix; clients add their own (Claude Code shows `mcp__brain__search`).

### 14.2 Output format

Every tool returns compact Markdown text, which is what the model reads. The core returns typed values and the MCP layer renders them; v1 declares no `outputSchema` and returns no `structuredContent`.

**Stored content is untrusted data.** Wherever `get`, `search` or `promotion` show text from a Recollection or Entity, it is wrapped in delimiters such as `<recollection id="…" untrusted>…</recollection>`, after a one-line note that the content is reference data, not instructions. `write` does not try to detect instruction-like text.

### 14.3 Annotations and loading

| Tool | Annotations |
|---|---|
| `context`, `search`, `get`, `history`, `promotion` | `readOnlyHint: true` (Claude Code runs these in parallel) |
| `write` | `destructiveHint: true` |
| `scan` | none (it only reconciles derived data) |
| `push` | `openWorldHint: true` |

`context` and `search` set `_meta["anthropic/alwaysLoad"]: true`; the rest are found through tool search. This is why `serve` answers the handshake before its startup sequence finishes (§2).

### 14.4 Output budgets

Budgets are constants in code, not owner configuration. They keep every result under Claude Code's 10,000-token warning.

| Tool | Budget |
|---|---|
| `search` | `limit` defaults to 10, maximum 25. No pagination: narrow the query instead |
| `get` | About 30,000 characters. Whole items are returned in request order until the next won't fit; the rest are listed under `omitted` to fetch again. An item over the budget on its own is truncated with a marker |
| `context` | About 4,000 characters. Lists are capped (e.g. 10 linked Entities, then "+N more"), with counts instead of lists where possible |
| `promotion` | About 30,000 characters, with items beyond it listed under `omitted` |
| errors | Under about 10,000 characters (Claude Code keeps only the first and last 5,000 of longer error text) |

**`get` on a Recollection** returns its front-matter, `version`, body, per-Anchor staleness (with relocated line ranges) and `docs` URLs resolved for the installed version. **`get` on an Entity** returns its fields, `version`, description, `docs` resolved for the version installed in the current Checkout, and the IDs and titles of up to 10 Recollections about it, then "+N more" and a hint to `search` with `entities: [id]`.

### 14.5 `write` results

**Success** lists, per operation, an outcome line with any new IDs (and the refs they replace) and the new `version` of every file touched, then warnings (Anchors on uncommitted lines, Anchors relocated), then the commit SHA.

**Rejection** lists every problem from §5.2 with its operation index and check, and for duplicates the candidates and decision table.

### 14.6 Errors

Domain failures are tool results with `isError: true`; JSON-RPC errors are only for protocol problems, such as input that fails the tool's JSON Schema. Each domain error starts with a stable code and ends with one line saying what to do next.

| Code | When |
|---|---|
| `rejected` | A `write` batch failed its checks (§5.2) |
| `conflict` | A `version` no longer matches the file (reported within a rejection) |
| `not_found` | An ID passed to `get`, `history` or `promotion` doesn't exist |
| `busy` | The startup sequence or the write lock didn't finish within the timeout |
