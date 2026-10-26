# Development happens in a dev container; CI runs natively

Every build, test and agent session runs inside the repo's dev container, so the development environment is fully reproducible, and the host only needs Docker and `@devcontainers/cli` (`up` and `exec` give CLI access from outside it). CI does not run in the container: it runs natively on Linux and macOS runners, so both shipping platforms are tested as they really are before a release, and a separate job builds the container and runs the checks inside it only when the environment itself changes. We accept keeping the toolchain in two places in step (the container and the native runners), which is why Rust is pinned in `rust-toolchain.toml`, read by rustup in both, rather than in the container image alone.

## Considered Options

- **Host toolchain managed by mise:** reproducible versions, but not a reproducible system (libraries, shell, OS), and agents could drift from it.
- **CI inside the dev container:** perfect parity on Linux, but macOS would never be built natively before release.
