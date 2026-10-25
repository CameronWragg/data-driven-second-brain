# rmcp maturity and the stdio shutdown lifecycle

Research for issue #19. Checked 2026-10-25 against primary sources only: the `rmcp` source at `modelcontextprotocol/rust-sdk` (commit `8aec389`, 2026-10-23), crates.io, the MCP specification (revisions `2026-07-28` and `2025-11-25`), Claude Code's MCP documentation, and the TypeScript SDK source (commit `951fe56`, 2026-10-23).

## Answer in brief

- **rmcp is fit for `brain serve`.** It is the official SDK, at stable 3.7.0, with semver guarantees, 100% conformance on both current spec suites, and support for server instructions, tool descriptions, JSON Schema tool inputs and structured outputs.
- **The shutdown push in SPEC §2 cannot be relied on, and under Claude Code on Linux and macOS it effectively never runs.** Claude Code does not close stdin first on POSIX. It sends `SIGINT`, then `SIGTERM` 100 ms later, then `SIGKILL` 400 ms after that, so a server gets at most about 500 ms. A `git pull --rebase` and `git push` over the network does not fit in that window. With no signal handler, Rust's default `SIGINT` action ends the process at once and no shutdown code runs.
- **SPEC change this forces:** push after each successful `write` batch while the session is still live, keep the startup push as the catch-up, and treat shutdown as "exit fast and do no network work". ADR-0005 needs amending to match. Details are in [Implications for the SPEC](#implications-for-the-spec).

## 1. rmcp maturity

| Fact | Source |
|---|---|
| The latest version is **3.7.0**, published 2026-10-22, and it is also the latest stable version. 3.0.0 shipped on 2026-07-28, the day the `2026-07-28` spec was released. There have been 15 releases since then, about one a week. The crate has had roughly 40M downloads in total. | crates.io API `GET /api/v1/crates/rmcp` |
| It follows SemVer 2.0. `rmcp` and `rmcp-macros` are released together. Public structs and enums are `#[non_exhaustive]`. `cargo-semver-checks` runs in release automation, and raising the MSRV counts as a breaking change. | `VERSIONING.md` |
| It meets every SEP-1730 **Tier 1** requirement: 100% server conformance (30/30) and 100% client conformance on both the `2025-11-25` and `2026-07-28` suites, triage and P0 SLAs met, and 48 out of 48 features documented. | `ROADMAP.md` |
| It "implements the stable MCP `2026-07-28` specification while remaining fully compatible with the `2025-11-25` release and earlier". `ProtocolVersion::LATEST = V_2026_07_28` and `LATEST_WITH_INITIALIZE = V_2025_11_25`. | `README.md`; `crates/rmcp/src/model.rs` |
| The **server is dual-era**: it answers a legacy `initialize` handshake, and it also serves modern requests that carry their metadata inline in `_meta`, including `server/discover`. | `crates/rmcp/src/service/server.rs` (version fallback and the `DiscoverRequest` handling) |
| The default features are `base64`, `macros` and `server`. A stdio server also needs `transport-io`. | `crates/rmcp/Cargo.toml` |

### Features the Second Brain needs

- **Server instructions:** `ServerConfig::with_instructions(..)`, returned from `ServerHandler::get_info`. It is sent in the `initialize` result for legacy clients and in `DiscoverResult.instructions` for modern ones. (`model.rs`)
- **Tool descriptions and JSON Schema inputs:** `#[tool_router]`, `#[tool(name, description)]`, and `Parameters<T>` where `T: JsonSchema` (schemars 1.x) generate each tool's `inputSchema`. (`examples/servers/src/structured_output.rs`)
- **Structured output:** returning `Json<T>` with `T: JsonSchema` produces `structuredContent` and generates the tool's `outputSchema`. (Same example and `handler/server/wrapper/json.rs`.)

## 2. What the MCP spec says about stdio shutdown

**Current revision (`2026-07-28`, stateless):** there is no `initialize` and no session. Every request carries its version and capabilities in `_meta`, and servers MUST implement `server/discover`. From the stdio binding page, *Shutdown*:

> The client **SHOULD** initiate shutdown by: 1. Closing the input stream to the child process (the server). 2. Waiting for the server to exit. 3. If the server does not exit within a reasonable time, forcibly terminating the process … On POSIX systems, forced termination typically escalates from `SIGTERM` to `SIGKILL`.
>
> Servers **SHOULD** exit promptly when their standard input is closed or reads return end-of-file. This is the primary graceful-shutdown signal and the only portable one…
>
> *Unexpected Termination:* If the server process exits unexpectedly, the client **SHOULD** restart it. Because the protocol is stateless, any in-flight requests are simply lost…

Source: <https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio>

**Legacy revision (`2025-11-25`):** the lifecycle page has the same three steps (close stdin, wait or `SIGTERM`, then `SIGKILL`), and "No specific shutdown messages are defined". Source: <https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle>

Neither revision gives a time limit. Both are written as **SHOULD**, so clients may do otherwise, and Claude Code does.

## 3. How clients actually stop stdio servers

| Client | What it does at shutdown | Grace before `SIGKILL` | Source |
|---|---|---|---|
| **Claude Code, macOS and Linux** | Sends `SIGINT`, then `SIGTERM` after 100 ms, then `SIGKILL` after a further 400 ms. **It does not close stdin first.** | about 500 ms in total | code.claude.com/docs/en/mcp, "How Claude Code stops a stdio server" |
| **Claude Code, Windows** | Closes stdin, then ends the process tree after 300 ms. Before v2.1.296 it ended the tree immediately. | 300 ms | same |
| **TypeScript SDK `StdioClientTransport`**, used by many Node-based hosts | Calls `stdin.end()`, waits 2 s, sends `SIGTERM`, waits 2 s, then sends `SIGKILL`. | about 4 s | `packages/client/src/client/stdio.ts`, `close()` |
| **rmcp `TokioChildProcess`** (Rust clients) | Drops stdin, waits 3 s, then kills the process. It never sends `SIGTERM`. | 3 s | `crates/rmcp/src/transport/child_process.rs`, `graceful_shutdown`, `MAX_WAIT_ON_DROP_SECS = 3` |

Claude Code also documents these behaviours:

- It sets `CLAUDE_PROJECT_DIR` in the server's environment to the project root, "so your server can resolve project-relative paths without depending on the working directory". It also answers `roots/list` with the launch directory plus every `--add-dir`.
- When a stdio server exits mid-session, an interactive session marks it failed and does not restart it; the user has to choose Reconnect in `/mcp`. `claude -p` runs and Agent SDK sessions restart it on the next tool call.
- The default limit for server instructions, and for the description of a tool that loads upfront, is 4,096 characters. A tool loaded through tool search gets 16,384 characters. Text past the limit is cut off. With tool search, "only tool names and server instructions load at session start".
- Tool output produces a warning above 10k tokens and is capped at 25k tokens by default. Text results over 50,000 characters are saved to a file instead.

Other situations, which are not client-specific: if the host crashes or is `SIGKILL`ed, the pipe closes and the server reads EOF on stdin. If the terminal itself closes, `SIGHUP` may arrive instead, depending on process-group placement, which Claude Code's docs don't specify.

## 4. What rmcp does at shutdown

- **stdin EOF:** the serve loop logs "input stream terminated" and stops with `QuitReason::Closed`. It then waits up to 5 s for in-flight handler responses, cancels the remaining handlers and closes the transport. `RunningService::waiting()` returns, so code in `main` after `service.waiting().await` **does run** on EOF. (`crates/rmcp/src/service.rs`)
- **Cancellation** (`RunningService::close`/`cancel`/`close_with_timeout`, or a `CancellationToken` passed in) drains for up to 2 s, then shuts down the same way.
- **Signals:** rmcp installs **no** signal handlers. There is no `ctrl_c` or `SIGTERM` handling anywhere in `crates/rmcp/src`, and the stdio example simply calls `service.waiting().await`. If the server does nothing about signals, Rust's default action applies: `SIGINT` and `SIGTERM` end the process immediately, without running destructors or code after `waiting()`. To run shutdown work, the server must `tokio::select!` on `waiting()` together with `tokio::signal::unix::signal(SIGINT | SIGTERM | SIGHUP)`, then call `close_with_timeout`.
- `transport::stdio_pipes()` on Linux puts the stdio FIFOs into non-blocking mode. It is a drop-in replacement for `stdio()`.

## 5. Is a shutdown push reliable?

**No.** Each case below rules it out on its own:

1. **Claude Code on POSIX**, the primary target, allows about 500 ms and never closes stdin. Even with a signal handler, `git pull --rebase` followed by `git push` to a remote usually takes longer than that because of SSH or HTTPS round-trips. The push would be cut off by `SIGKILL`, possibly part-way through the rebase.
2. **No handler means no shutdown code.** With rmcp's defaults, `SIGINT` ends the process before any shutdown push can start.
3. **Host crash, `SIGKILL` or power loss** gives the server nothing to react to.
4. **Clients that are more generous** (about 3 to 4 s for the TS SDK and rmcp) close stdin first, so a push would *sometimes* finish there. That still is not a guarantee.

What does hold up when the process is killed:

- **The `flock` write lock** (SPEC §11) is released by the kernel when the process dies.
- **SQLite in WAL mode** is crash-safe.
- **Git is the risk.** A `git` child process killed part-way can leave `.git/index.lock` or a `rebase-merge/` directory behind. Signals from Claude Code target the server's PID, so a `git` child that keeps running may even finish after the server has died.

## Implications for the SPEC

1. **§2 and §10: push after every successful `write` batch.** Do it in-process, right after the commit and still under the session, without blocking the tool response. This could run as a tokio task, with the result reported through `context` and any failures retried on the next write or `push`. This makes the startup push a real catch-up rather than the only dependable one. The explicit `push` tool stays.
2. **§2 Shutdown: replace "push any unpushed commits" with "exit promptly".** On stdin EOF, `SIGINT`, `SIGTERM` or `SIGHUP`, stop accepting requests, let an in-flight commit finish if it can within about 100 ms, and exit. Do no network work. Install the signal handlers explicitly, because rmcp does not.
3. **ADR-0005: amend the push triggers** to "on `push`, after each write, and on startup", and drop "on shutdown".
4. **§2 startup: recover from interrupted git state** before step 2.1. Treat a stale `.git/index.lock` as abandoned once its owner is gone (the brain's own lock is already held). If a rebase was left in progress, abort it and then pull again.
5. **§2 step 3: Checkout detection.** Prefer `CLAUDE_PROJECT_DIR` when it is set, then the working directory. The server's working directory depends on which config scope declared it, so it is not a reliable project root. Note that `roots/list` is a server-to-client request, which the stateless `2026-07-28` protocol no longer allows outside multi round-trip requests.
6. **§14 / tool design: keep instructions and descriptions within Claude Code's limits.** Server instructions should stay under 4,096 characters, with the important text first. Large `get` results should stay well under 25k tokens or 50k characters.
7. **Protocol era.** Build on rmcp 3.x as a dual-era server, so that both modern (`2026-07-28`) and legacy (`initialize`) clients work. The stateless model fits ADR-0005 well, because each process can be restarted at any time without losing anything.

## Open questions this raises

- **Process group for `git` children:** should `brain` put its `git` children in its own process group (or use `PR_SET_PDEATHSIG` on Linux) so they die with it, or let an in-flight push finish on its own? Letting it run on is strictly a short-lived orphan process, which sits uneasily with ADR-0005.
- **Push latency and conflicts:** per-write pushes call `pull --rebase` more often. Should pushes be debounced within a session (for example, at most one in flight, coalescing later writes), and how should a push failure be shown, through `context` or in the `write` result?
- **Interactive Claude Code does not restart a crashed server.** Is a "server exited" state visible enough to the owner, or should `brain` be extra careful never to exit on recoverable errors?
- **Windows:** Claude Code gives 300 ms after closing stdin there. If v1 doesn't target Windows, the SPEC should say so.

## Sources

- rmcp source: <https://github.com/modelcontextprotocol/rust-sdk> at `8aec389`, specifically `README.md`, `VERSIONING.md`, `ROADMAP.md`, `crates/rmcp/Cargo.toml`, `crates/rmcp/src/{service.rs,service/server.rs,model.rs,transport/io.rs,transport/child_process.rs}` and `examples/servers/src/{counter_stdio.rs,structured_output.rs}`
- crates.io: <https://crates.io/crates/rmcp>
- MCP stdio binding, `2026-07-28`: <https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio>
- MCP versioning and backward compatibility, `2026-07-28`: <https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning>
- MCP lifecycle, `2025-11-25`: <https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle>
- Claude Code MCP docs: <https://code.claude.com/docs/en/mcp>, in particular "How Claude Code stops a stdio server", "Stdio servers that exit or fail to start", the tool-search description limits and the output limits
- TypeScript SDK stdio client: <https://github.com/modelcontextprotocol/typescript-sdk/blob/main/packages/client/src/client/stdio.ts> at `951fe56`
