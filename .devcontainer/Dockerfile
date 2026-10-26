# The development environment (ADR-0006). Built with the repository root as
# context so the toolchain pin and tool versions are baked into the image.
FROM mcr.microsoft.com/devcontainers/base:trixie@sha256:6ee2b4097c75d33e9a94d941577a3286e5d629434fba384688b06daeeaaeac99

ARG TARGETARCH

ARG NODE_VERSION=24.22.1
ARG NODE_SHA256_AMD64=6c172a9ab6718f7b801fb5a7bd486af2ee5e32758086a04b84fcce2aa77095e6
ARG NODE_SHA256_ARM64=dbabf7a1cbf947e6059ffad2c27b80041db97dea1e553b62afdee7f13fca336e

ARG RUSTUP_VERSION=1.29.2
ARG RUSTUP_SHA256_AMD64=5be191e8c70a5007074254cb74d78dbfdd25cd5a61c27de7b8e5410122ec24f4
ARG RUSTUP_SHA256_ARM64=a9c168dbea81a84671c6b7a5c1795cb0eb699020110d107ddfca427bd797e697

ARG CLAUDE_CODE_VERSION=2.1.308

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential gh jq pkg-config xz-utils \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) arch=x64; sha="$NODE_SHA256_AMD64" ;; \
      arm64) arch=arm64; sha="$NODE_SHA256_ARM64" ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSLo /tmp/node.tar.xz "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-${arch}.tar.xz"; \
    echo "$sha  /tmp/node.tar.xz" | sha256sum -c -; \
    tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 --exclude='*.md' --exclude=LICENSE; \
    rm /tmp/node.tar.xz; \
    npm install -g "@anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}"; \
    npm cache clean --force

USER vscode

# rustup installs the toolchain rust-toolchain.toml names, so a pin bump
# rebuilds the image. The cargo registry, ~/.cache and target/ are named
# volumes (devcontainer.json); creating them here gives them the right owner.
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-gnu; sha="$RUSTUP_SHA256_AMD64" ;; \
      arm64) target=aarch64-unknown-linux-gnu; sha="$RUSTUP_SHA256_ARM64" ;; \
    esac; \
    curl -fsSLo /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${target}/rustup-init"; \
    echo "$sha  /tmp/rustup-init" | sha256sum -c -; \
    chmod +x /tmp/rustup-init; \
    /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain none; \
    rm /tmp/rustup-init; \
    mkdir -p "$HOME/.cargo/registry" "$HOME/.cache"

ENV PATH="/home/vscode/.cargo/bin:${PATH}"

WORKDIR /tmp/latens-toolchain
COPY --chown=vscode rust-toolchain.toml ./
RUN rustup toolchain install

COPY --chown=vscode .config/tool-versions.toml .config/
COPY --chown=vscode scripts/install-tools.sh scripts/
RUN scripts/install-tools.sh

WORKDIR /
RUN rm -rf /tmp/latens-toolchain
