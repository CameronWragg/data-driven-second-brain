# PROTOTYPE: how Agents are taught to use the brain

Throwaway. Answers [Prototype how Agents are taught to use the brain](https://github.com/CameronWragg/data-driven-second-brain/issues/25). Never merged.

Four candidate channels, drafted as real text:

| File | Channel | Seen by | Cost |
|---|---|---|---|
| `1-server-instructions.md` | MCP server `instructions` (3,119 / 4,096 chars) | every client, every session | none: it's a string in `serve` |
| `2-tool-descriptions.md` | tool descriptions | `context`/`search` always; others via tool search | none |
| `3-agents-md-snippet.md` | snippet printed by `brain init` | only if the owner pastes it | none |
| `plugin/` | Claude Code plugin: `.mcp.json`, `/brain:remember` skill, Stop hook | Claude Code users who install it | a plugin to package and a `brain hook stop` subcommand |

The Stop hook (`plugin/hooks/stop-nudge.sh`) was run against 12 real local transcripts. It nudged in 3, all sessions with 3 or more file edits and no brain write, and stayed silent in the others. It nudges at most once per new batch of edits and never on its own continuation (`stop_hook_active`).
