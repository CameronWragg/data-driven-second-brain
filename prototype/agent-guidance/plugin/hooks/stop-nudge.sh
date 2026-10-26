#!/usr/bin/env bash
# PROTOTYPE Stop hook. In v1 this would be `brain hook stop`: a short-lived
# client-side process, so ADR-0005 holds (nothing runs in `serve`).
#
# Nudge at most once per stretch of work, and only when:
#   - this isn't already a nudged continuation (stop_hook_active), and
#   - code was edited since the last brain `write` (or since the session began), and
#   - the edit count since then reaches a threshold (skip trivial one-line fixes).
set -euo pipefail
input=$(cat)
[[ $(jq -r '.stop_hook_active // false' <<<"$input") == true ]] && exit 0
transcript=$(jq -r '.transcript_path' <<<"$input")
[[ -f $transcript ]] || exit 0

# Tool calls in order, one name per line.
names=$(jq -r 'select(.type=="assistant") | .message.content[]? | select(.type=="tool_use") | .name' "$transcript")

# Count edits since the last brain write.
edits=$(awk '
  /^mcp__.*brain__write$/ { n = 0; next }
  /^(Edit|Write|MultiEdit|NotebookEdit)$/ { n++ }
  END { print n + 0 }' <<<"$names")

# Remember the count at the last nudge, so a declined nudge isn't repeated
# every turn; only new edits on top of it re-arm the hook.
state=${XDG_STATE_HOME:-$HOME/.local/state}/brain-prototype/nudge-$(jq -r .session_id <<<"$input")
last=$(cat "$state" 2>/dev/null || echo 0)
(( edits < last )) && last=0   # a brain write reset the count
threshold=${BRAIN_NUDGE_MIN_EDITS:-3}
(( edits - last >= threshold )) || exit 0
mkdir -p "$(dirname "$state")" && echo "$edits" >"$state"

jq -n --arg n "$edits" '{
  decision: "block",
  reason: ("You made \($n) file edits since the last Second Brain write. Before finishing: did this task produce a decision, convention, caveat, architecture fact or how-to that a future session would have to rediscover and that is NOT obvious from the code? If yes, search the brain, then record it with `write`. If not, finish without writing and say nothing about the brain.")
}'
