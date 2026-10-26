# The Second Brain does nothing unless an Agent is connected

The Second Brain runs only as an MCP server launched by an Agent session: no daemons, file watchers, idle timers or scheduled syncs. Rescans happen on startup when manifests have changed or on an explicit `scan`; commits happen per `write` call; pushes happen in the background after each successful `write` batch, on an explicit `push`, and again on the next startup to catch anything a killed process left unpushed. There is no push at shutdown: Claude Code stops stdio servers with `SIGINT`, `SIGTERM` and then `SIGKILL` within about 500 ms without closing stdin, so a shutdown push would almost never run. Shutdown is a prompt, best-effort exit, and correctness rests on a write journal and recovery at the next startup instead. `git` children die with `serve` on Linux and are bounded by a timeout on macOS. We accept that changes made between sessions (hand edits, upstream merges, unpushed commits) are only picked up when the next session starts, in exchange for a tool that consumes nothing while idle.

## Considered Options

- **Push on shutdown** (the original decision): never reliable under Claude Code's signal timing, and impossible after a crash or `SIGKILL`.
- **A detached helper that pushes after `serve` exits:** reliable, but it is exactly the background process this decision rules out.
- **Push on a timer or debounce within the session:** a timer for no gain; one push in flight with one follow-up queued already coalesces bursts of writes.
