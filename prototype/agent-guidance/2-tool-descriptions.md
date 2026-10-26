# Tool descriptions (PROTOTYPE)

`context` and `search` load upfront (`alwaysLoad`), so their descriptions are always in the prompt. The others are found by **tool search**, which matches on the tool name and description, so those descriptions lead with the words an Agent would search for ("save", "remember", "record", "fetch", "read full").

Rule of thumb: the **instructions** say *when* to use a tool, and the **description** says *what it does and how to call it*. The MCP spec says instructions shouldn't repeat the descriptions, so the descriptions don't restate the policy.

---

### `context`
Start here. Shows what the Second Brain knows about where you are: the current Repository, Branch and Checkout, the Repositories and Dependencies directly Linked to it, and anything that needs attention (pending Promotions, Branches with unknown status, stale Recollections, files needing repair, sync problems). Takes an optional `path` to re-detect the Checkout after you change directory. Read-only.

### `search`
Search the Second Brain's Recollections (decisions, conventions, caveats, architecture notes, how-tos) and Entities by meaning and keyword. Returns up to 10 one-line summaries (max 25) with id, kind, title, Branch, staleness and `version`. Narrow with `kinds`, `entities`, `tags` or `branches`. `scope` is `current` (the default: this Repository and its Dependencies), `all`, or a Repository id. Use `get` for the full text. Read-only.

### `get`
Fetch and read the full text of Recollections or Entities by id: front-matter, body, `version`, per-Anchor staleness, and documentation URLs for the installed Dependency version. Accepts several ids. Items that don't fit the budget are listed under `omitted`. Read-only.

### `write`
Save, record or remember knowledge in the Second Brain, or change it: create, update, supersede, delete or link Recollections, create or update Entities, and settle a Branch's status. Takes an atomic batch of `operations`: every check runs first, and nothing is written unless all pass. Changes to existing items need the `version` you last saw. A rejection lists every problem at once (schema, secret, conflict or duplicate). Each successful batch is one git commit.

### `promotion`
Get the plan for moving a merged Branch's Recollections into the branch it was merged into: each Recollection with overlapping ones already on the target, target Recollections whose code changed in the merge, and re-checked Anchors. Resolve each item with `write`. Read-only.

### `history`
Show the git history of a Recollection or Entity, including superseded and deleted ones, or read it as it was at a given `commit`. Use it to recover or audit past knowledge. Read-only.

### `scan`
Re-scan the current Checkout's dependency manifests (package.json, Cargo.toml, pyproject.toml, go.mod and their lockfiles) and update the Repository's Dependencies and Links. This runs automatically at startup when a manifest has changed.

### `push`
Sync the Second Brain to its private git remote now: pull with rebase, then push.
