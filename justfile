set shell := ["bash", "-euo", "pipefail", "-c"]

# Dev runs keep their brain, state and config in the git-ignored .dev/, away from
# the real brain. XDG_CACHE_HOME stays shared so model weights aren't fetched again.
dev := justfile_directory() / ".dev"
dev_env := "XDG_DATA_HOME=" + dev / "data" + " XDG_STATE_HOME=" + dev / "state" + " XDG_CONFIG_HOME=" + dev / "config"
bin := justfile_directory() / "target/debug/latens"

# The MCP Inspector `just inspect` runs; bumped by hand.
inspector := "@modelcontextprotocol/inspector@2.10.1"

# List the recipes
default:
    @just --list

# The full local gate; must pass before a PR
check: && test
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo deny check

# Run the tests
test *args:
    cargo nextest run {{ args }}
    cargo test --doc

# Run `latens serve` over stdio against the dev brain
dev-serve: build
    env {{ dev_env }} {{ bin }} serve

# Start Claude Code with only the dev build's MCP server, named brain-dev
dev-claude *args: build
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p {{ dev }}
    cat > {{ dev }}/mcp.json <<JSON
    {
      "mcpServers": {
        "brain-dev": {
          "type": "stdio",
          "command": "{{ bin }}",
          "args": ["serve"],
          "env": {
            "XDG_DATA_HOME": "{{ dev / "data" }}",
            "XDG_STATE_HOME": "{{ dev / "state" }}",
            "XDG_CONFIG_HOME": "{{ dev / "config" }}"
          }
        }
      }
    }
    JSON
    exec claude --mcp-config {{ dev }}/mcp.json --strict-mcp-config {{ args }}

# Run the MCP Inspector against `latens serve` on the dev brain: the web UI on
# port 6274, or pass `--cli --method tools/list` and so on for the CLI
inspect *args: build
    npx -y {{ inspector }} {{ args }} \
        -e XDG_DATA_HOME={{ dev / "data" }} -e XDG_STATE_HOME={{ dev / "state" }} -e XDG_CONFIG_HOME={{ dev / "config" }} \
        {{ bin }} serve

# Delete the dev brain, state and config
dev-reset:
    rm -rf {{ dev }}

[private]
build:
    cargo build
