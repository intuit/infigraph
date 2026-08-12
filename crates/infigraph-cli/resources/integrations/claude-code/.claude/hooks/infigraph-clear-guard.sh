#!/usr/bin/env bash
# Infigraph UserPromptSubmit hook — block /clear unless session was saved.
# Uses a separate sentinel set by session-reset hook after save_session.
# Sentinel is cleared on session start so each session must save at least once.

input=$(cat)
prompt=$(echo "$input" | jq -r '.prompt // empty')
cwd=$(echo "$input" | jq -r '.cwd // empty')
session_id=$(echo "$input" | jq -r '.session_id // empty')

# Only guard infigraph-indexed projects
[ -d "$cwd/.infigraph" ] || exit 0

# Check if prompt is /clear
cleaned=$(echo "$prompt" | sed 's/^[[:space:]]*//' | sed 's/[[:space:]]*$//')
[ "$cleaned" = "/clear" ] || exit 0

# Check if save_session was called this session
saved_file="${TMPDIR:-/tmp}/infigraph-sessions/$session_id.saved"
if [ -f "$saved_file" ]; then
  exit 0
fi

cat <<'ENDJSON'
{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","decision":"block","reason":"⚠️ Session not saved! Call save_session first, then /clear. Unsaved context will be lost."}}
ENDJSON

exit 0
