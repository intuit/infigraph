#!/usr/bin/env bash
# Infigraph UserPromptSubmit hook — session save reminder
# Counts user exchanges per Claude session. Every 10th exchange, emits a
# reminder to call save_session. Resets when the PostToolUse reset hook fires.
# stdin: JSON {prompt, session_id, cwd, ...}
#
# MCP-liveness aware: a hook script cannot query Claude Code's actual MCP
# connection state (not exposed to hooks), so `pgrep` for a running
# infigraph-mcp process is used as a cheap ONE-SIDED heuristic: no process
# found is high-confidence "the tool call would fail," but a process found
# is NOT proof the tool is reachable from THIS session (could be a stale,
# orphaned, or another session's worker) — pgrep can only rule the tool
# OUT, never confirm it's IN. So the real gate against wasted tokens is
# always a cheap ToolSearch check done BEFORE composing any narrative,
# regardless of what pgrep found; pgrep only changes the up-front framing.

input=$(cat)
cwd=$(echo "$input" | jq -r '.cwd // empty')

# Only enforce in Infigraph-indexed projects
[ -d "$cwd/.infigraph" ] || exit 0

session_id=$(echo "$input" | jq -r '.session_id // empty')
[ -z "$session_id" ] && exit 0

# Teammate/subagent relays (SendMessage-delivered turns, e.g. idle
# notifications and status pings from background agents) arrive as
# UserPromptSubmit events just like real human input, but aren't a "user
# exchange" in the sense this hook means -- exclude them so the counter
# tracks actual turns with the human, not agent-orchestration chatter.
prompt=$(echo "$input" | jq -r '.prompt // empty')
case "$prompt" in
  *"<teammate-message"*) exit 0 ;;
esac

counter_dir="${TMPDIR:-/tmp}/infigraph-sessions"
mkdir -p "$counter_dir" 2>/dev/null
counter_file="$counter_dir/$session_id.count"

count=0
[ -f "$counter_file" ] && count=$(cat "$counter_file" 2>/dev/null || echo 0)
count=$((count + 1))
echo "$count" > "$counter_file"

if [ $((count % 10)) -eq 0 ]; then
  if pgrep -f "infigraph-mcp" >/dev/null 2>&1; then
    mcp_hint="An infigraph-mcp process is running, but that does NOT prove it's connected to this session (could be stale, orphaned, or serving a different session)."
  else
    mcp_hint="No infigraph-mcp process detected at all -- mcp__infigraph__save_session is essentially guaranteed to fail."
  fi

  check_first="Regardless of that: before writing ANY summary content, do a cheap availability check first (ToolSearch for mcp__infigraph__save_session, or just attempt the call once with minimal args) -- do NOT compose the summary until you've confirmed the tool actually responds. This check is what actually prevents wasted work, not the process-liveness hint above."

  fallback_recipe="FALLBACK once the check above shows the tool is unavailable or it errors: write directly to a file at \$cwd/.infigraph/sessions/session_<YYYY-MM-DD>.json (today's UTC date). It's a JSON object with these string fields: id (value 'session_<YYYY-MM-DD>'), name (empty string unless this is a named session), summary, pending_tasks, decisions, files_touched (comma-separated), constraints, assumptions, blockers -- plus these numeric fields: created_at, updated_at, last_accessed (all unix epoch seconds), and confidence (float). If a file for today already exists, load and merge into it rather than overwriting: decisions appends onto the existing value separated by ' | ', files_touched unions the new files in, everything else (summary/pending_tasks/constraints/assumptions/blockers) replaces the old value, created_at is kept from the existing file, confidence becomes the max of 0.9 and the existing confidence, updated_at and last_accessed become now. Do NOT include narrative on this periodic save -- structured fields only, this is a cheap safety-net save, not a milestone save. Then, to keep /clear working correctly (it checks a sentinel that's normally only set after a real tool call succeeds), write '0' to \${TMPDIR:-/tmp}/infigraph-sessions/\$session_id.count and '1' to \${TMPDIR:-/tmp}/infigraph-sessions/\$session_id.saved. Only use this fallback once the check above has actually shown the tool is unreachable -- never skip the check itself."

  full_context="MANDATORY AUTO-SAVE (periodic backstop): Print '💾 Auto-saving session...' to the user, then save this session's context -- summary, pending_tasks, and decisions -- before answering the user's question. Do NOT include narrative on this periodic save. $mcp_hint $check_first $fallback_recipe After saving (via either path), print '✅ Session saved.' and continue with the user's request. This is automatic -- do NOT ask permission, do NOT skip, do NOT defer, and do NOT silently give up if the tool call fails without falling back."

  json_escaped=$(printf '%s' "$full_context" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read())[1:-1])')

  cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"$json_escaped"}}
ENDJSON
fi

exit 0
