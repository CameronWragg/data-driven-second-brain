#!/usr/bin/env bash
# Fails if a doc names a `just` recipe that doesn't exist, so the docs can name
# recipes without copying their commands and never drift from the justfile.
#
# Usage: scripts/check-recipes.sh [file...]   (default: AGENTS.md and README.md)
# Only code counts: `just <recipe>` inside an inline code span or a fenced block.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"

if [[ $# -eq 0 ]]; then
  for doc in "$root/AGENTS.md" "$root/README.md"; do
    if [[ -f "$doc" ]]; then set -- "$@" "$doc"; fi
  done
fi
if [[ $# -eq 0 ]]; then echo "check-recipes: no docs to check" >&2; exit 1; fi

recipes=" $(just --justfile "$root/justfile" --summary) "
missing=0

# Prints `file:line:recipe` for every `just <recipe>` in code. A recipe name
# starts with a letter, so placeholders (`just <recipe>`) and flags
# (`just --list`) are skipped.
mentions() {
  awk '
    /^[[:space:]]*(```|~~~)/ { fenced = !fenced; next }
    {
      if (fenced) { code = $0 }
      else {
        code = ""; line = $0
        while (match(line, /`[^`]+`/)) {
          code = code " " substr(line, RSTART, RLENGTH)
          line = substr(line, RSTART + RLENGTH)
        }
      }
      while (match(code, /(^|[^[:alnum:]_-])just +[[:alpha:]][[:alnum:]_-]*/)) {
        mention = substr(code, RSTART, RLENGTH)
        sub(/^.*just +/, "", mention)
        print FILENAME ":" FNR ":" mention
        code = substr(code, RSTART + RLENGTH)
      }
    }
  ' "$@"
}

while IFS=: read -r file line recipe; do
  if [[ "$recipes" != *" $recipe "* ]]; then
    echo "$file:$line: no recipe \`$recipe\` in the justfile" >&2
    missing=1
  fi
done < <(mentions "$@")

if [[ $missing -eq 0 ]]; then echo "check-recipes: every recipe named in $* exists"; fi
exit "$missing"
