#!/usr/bin/env bash
# Installs the dev tools named in .config/tool-versions.toml from their upstream
# release binaries, refusing any download whose SHA-256 doesn't match (ADR-0007).
#
# Usage: scripts/install-tools.sh [tool...]   (default: every tool in the file)
# Installs into $INSTALL_DIR, default ${CARGO_HOME:-$HOME/.cargo}/bin.
set -euo pipefail

versions_file="${TOOL_VERSIONS:-$(cd "$(dirname "$0")/.." && pwd)/.config/tool-versions.toml}"
install_dir="${INSTALL_DIR:-${CARGO_HOME:-$HOME/.cargo}/bin}"

die() { echo "install-tools: $*" >&2; exit 1; }

# Prints the value of `key` in table `[table]`. The file is kept to the subset of
# TOML this understands: `[table]` headers and `key = "string"` lines.
toml_get() {
  local table="$1" key="$2" value
  value="$(awk -v table="$table" -v key="$key" '
    /^[[:space:]]*(#|$)/ { next }
    /^\[/ { current = $0; gsub(/^\[|\][[:space:]]*$/, "", current); next }
    current == table {
      line = $0
      sub(/^[[:space:]]*/, "", line)
      split(line, parts, /[[:space:]]*=[[:space:]]*/)
      if (parts[1] == key) {
        sub(/^[^=]*=[[:space:]]*"/, "", line)
        sub(/"[[:space:]]*$/, "", line)
        print line
        exit
      }
    }
  ' "$versions_file")"
  [[ -n "$value" ]] || die "no \`$key\` in [$table] of $versions_file"
  printf '%s\n' "$value"
}

platform() {
  local os arch
  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) die "unsupported OS: $(uname -s)" ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "unsupported architecture: $(uname -m)" ;;
  esac
  echo "$os-$arch"
}

sha256_of() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

install_tool() {
  local tool="$1" platform="$2" work="$3"
  local version url target expected actual archive binary
  version="$(toml_get "$tool" version)"
  target="$(toml_get "$tool.$platform" target)"
  expected="$(toml_get "$tool.$platform" sha256)"
  url="$(toml_get "$tool" url)"
  url="${url//\{version\}/$version}"
  url="${url//\{target\}/$target}"

  archive="$work/$tool.tar.gz"
  echo "install-tools: $tool $version ($target)"
  curl --proto '=https' --tlsv1.2 -fsSL --retry 3 -o "$archive" "$url"

  actual="$(sha256_of "$archive")"
  if [[ "$actual" != "$expected" ]]; then
    die "checksum mismatch for $tool $version ($target)
  expected $expected
  got      $actual
  from     $url"
  fi

  mkdir -p "$work/$tool"
  tar -xzf "$archive" -C "$work/$tool"
  binary="$(find "$work/$tool" -type f -name "$tool" | head -n 1)"
  [[ -n "$binary" ]] || die "no \`$tool\` binary in $url"
  mkdir -p "$install_dir"
  install -m 0755 "$binary" "$install_dir/$tool"
}

main() {
  [[ -f "$versions_file" ]] || die "missing $versions_file"
  local tools=("$@") platform
  if [[ ${#tools[@]} -eq 0 ]]; then
    mapfile -t tools < <(sed -n 's/^\[\([^].]*\)\][[:space:]]*$/\1/p' "$versions_file")
  fi
  platform="$(platform)"

  local work
  work="$(mktemp -d)"
  trap "rm -rf '$work'" EXIT
  for tool in "${tools[@]}"; do
    install_tool "$tool" "$platform" "$work"
  done
  echo "install-tools: installed ${tools[*]} into $install_dir"
}

main "$@"
