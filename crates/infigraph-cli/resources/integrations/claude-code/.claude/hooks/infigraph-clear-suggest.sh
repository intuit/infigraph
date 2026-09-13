#!/usr/bin/env bash
# Infigraph UserPromptSubmit hook — suggest /clear every 5 turns to keep context lean.
# Uses own counter file per session. Resets on session start (new session_id).

input=$(cat)
session_id=$(echo "$input" | jq -r '.session_id // empty')
[ -z "$session_id" ] && exit 0

counter_dir="${TMPDIR:-/tmp}/claude-clear-suggest"
mkdir -p "$counter_dir" 2>/dev/null
counter_file="$counter_dir/$session_id.count"

count=0
[ -f "$counter_file" ] && count=$(cat "$counter_file" 2>/dev/null || echo 0)
count=$((count + 1))
echo "$count" > "$counter_file"

if [ $((count % 5)) -eq 0 ]; then
  cat <<'ENDJSON'
{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"MANDATORY VISIBLE ACTION — CONTEXT CLEANUP: You MUST print this message verbatim to the user RIGHT NOW before doing anything else:\n\n---\n🧹 **Context getting long** — save session and type `/clear` to reset.\n---\n\nDo NOT skip this message. Do NOT silently absorb it. The user MUST see it in the chat output. After printing, continue with the user's request."}}
ENDJSON
fi

exit 0
