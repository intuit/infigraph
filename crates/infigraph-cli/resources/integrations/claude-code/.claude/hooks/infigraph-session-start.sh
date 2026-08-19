#!/usr/bin/env bash
# Infigraph SessionStart hook — session continuity on startup/resume/compaction
# stdin: JSON {session_id, cwd, source, ...}
# source: "startup" | "resume" | "clear" | "compact"
#
# MCP-liveness note: reads (get_latest_session) don't need a tool-call fallback
# at all -- this hook already reads .infigraph/sessions/*.json directly via
# inject_session_summary, so that content is injected up front regardless of
# whether the MCP tool would work. Only writes (save_session) genuinely need
# a try-then-fallback recipe, since only the LLM can compose new content.

input=$(cat)
cwd=$(echo "$input" | jq -r '.cwd // empty')

# Only enforce in Infigraph-indexed projects
[ -d "$cwd/.infigraph" ] || exit 0

source_type=$(echo "$input" | jq -r '.source // "startup"')
session_id=$(echo "$input" | jq -r '.session_id // empty')

# Clear saved sentinel on new session (must save before /clear)
if [ -n "$session_id" ]; then
  rm -f "${TMPDIR:-/tmp}/infigraph-sessions/$session_id.saved" 2>/dev/null
fi

# Helper: read latest session JSON and extract key fields inline
inject_session_summary() {
  local sessions_dir="$cwd/.infigraph/sessions"
  local latest=$(ls -t "$sessions_dir"/session_*.json 2>/dev/null | head -1)
  [ -z "$latest" ] && return
  local summary pending decisions constraints blockers
  summary=$(jq -r '.summary // empty' "$latest" 2>/dev/null)
  pending=$(jq -r '.pending_tasks // empty' "$latest" 2>/dev/null)
  decisions=$(jq -r '.decisions // empty' "$latest" 2>/dev/null)
  constraints=$(jq -r '.constraints // empty' "$latest" 2>/dev/null)
  blockers=$(jq -r '.blockers // empty' "$latest" 2>/dev/null)
  local session_id=$(basename "$latest" .json)
  local out="PRIOR SESSION ($session_id):"
  [ -n "$summary" ] && out="$out Summary: $summary."
  [ -n "$pending" ] && out="$out Pending: $pending."
  [ -n "$decisions" ] && out="$out Decisions: $decisions."
  [ -n "$constraints" ] && out="$out Constraints: $constraints."
  [ -n "$blockers" ] && out="$out Blockers: $blockers."
  local narrative="$sessions_dir/${session_id}.md"
  [ -f "$narrative" ] && out="$out Narrative log: $narrative (read if more context needed)."
  echo "$out"
}

# Shared save-fallback recipe (only the write side needs this -- reads are handled
# by inject_session_summary above, which already goes straight to the file with no
# tool call involved). pgrep can only rule the MCP tool OUT (no process = definitely
# unreachable), never confirm it's IN (a running process could be stale/orphaned/
# another session's) -- so it only changes the framing below. The actual gate against
# wasted work is always a cheap availability check done before composing any
# narrative, regardless of what pgrep found.
save_fallback_recipe() {
  local hint
  if pgrep -f "infigraph-mcp" >/dev/null 2>&1; then
    hint="An infigraph-mcp process is running, but that does NOT prove it's connected to this session (could be stale, orphaned, or serving a different session)."
  else
    hint="No infigraph-mcp process detected at all -- mcp__infigraph__save_session is essentially guaranteed to fail."
  fi
  echo "$hint Regardless of that: before writing ANY summary/narrative content, do a cheap availability check first (ToolSearch for mcp__infigraph__save_session, or attempt the call once with minimal args) -- do NOT compose the full narrative until you've confirmed the tool actually responds. FALLBACK once that check shows the tool is unavailable or it errors: write directly to a file at \$cwd/.infigraph/sessions/session_<YYYY-MM-DD>.json (today's UTC date). It's a JSON object with these string fields: id (value 'session_<YYYY-MM-DD>'), name (empty string unless this is a named session), summary, pending_tasks, decisions, files_touched (comma-separated), constraints, assumptions, blockers -- plus these numeric fields: created_at, updated_at, last_accessed (all unix epoch seconds), and confidence (float). If a file for today already exists, merge into it: decisions appends onto the existing value separated by ' | ', files_touched unions the new files in, everything else replaces the old value, created_at is kept from the existing file, confidence becomes the max of 0.9 and the existing confidence. Also append the narrative to the companion session_<YYYY-MM-DD>.md file as a new section (blank line, '## Save @ HH:MM UTC', blank line, narrative text). Then write '0' to \${TMPDIR:-/tmp}/infigraph-sessions/\$session_id.count and '1' to \${TMPDIR:-/tmp}/infigraph-sessions/\$session_id.saved so /clear keeps working. Only use this fallback once the check has actually shown the tool is unreachable -- never skip the check itself."
}

json_escape() {
  python3 -c 'import sys,json; print(json.dumps(sys.stdin.read().strip())[1:-1])'
}

case "$source_type" in
  compact)
    backup=$(ls -t "$cwd"/.infigraph/sessions/unsaved-transcript-*.md 2>/dev/null | head -1)
    session_ctx=$(inject_session_summary)
    [ -z "$session_ctx" ] && session_ctx="No prior sessions found."
    fallback=$(save_fallback_recipe)
    if [ -n "$backup" ]; then
      full="INFIGRAPH SESSION SAVE (COMPACTION): Context was just compacted. $session_ctx (that's what's already on disk from before this compaction -- no tool call needed to see it). A pre-compaction transcript backup exists at $backup. Read this file (contains recent exchanges as clean markdown), extract key context (summary, decisions, pending tasks, constraints, assumptions, blockers, files touched) not yet captured above, then persist it: $fallback After saving (via either path), delete the backup file with Bash rm. Then call mcp__infigraph__get_latest_session to reload saved context. Do NOT skip this."
    else
      full="INFIGRAPH SESSION SAVE (COMPACTION): Context was just compacted. $session_ctx (that's what's already on disk from before this compaction -- no tool call needed to see it). Pre-compaction decisions and context not yet captured above are at risk of being lost -- capture them now. $fallback Then call mcp__infigraph__get_latest_session to reload saved context. Do NOT skip this."
    fi
    escaped=$(printf '%s' "$full" | json_escape)
    cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"$escaped"}}
ENDJSON
    ;;
  startup|resume)
    session_ctx=$(inject_session_summary)
    if [ -n "$session_ctx" ]; then
      escaped=$(printf '%s' "$session_ctx" | json_escape)
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"INFIGRAPH SESSION RESTORE: $escaped You MUST print a visible summary of prior session context to the user (summary + pending tasks) so they can see session continuity is working."}}
ENDJSON
    else
      cat <<'ENDJSON'
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"INFIGRAPH SESSION RESTORE: No prior sessions found. Fresh start."}}
ENDJSON
    fi
    ;;
  clear)
    # Reset exchange counters on /clear
    session_id=$(echo "$input" | jq -r '.session_id // empty')
    if [ -n "$session_id" ]; then
      echo "0" > "${TMPDIR:-/tmp}/infigraph-sessions/$session_id.count" 2>/dev/null
      echo "0" > "${TMPDIR:-/tmp}/claude-clear-suggest/$session_id.count" 2>/dev/null
    fi
    # Reset test-context sentinel on clear
    rm -f "$cwd/.infigraph/.test-context-called" 2>/dev/null
    rm -f "$cwd/.infigraph/.search-fallback-allowed" 2>/dev/null
    backup=$(ls -t "$cwd"/.infigraph/sessions/unsaved-transcript-*.md 2>/dev/null | head -1)
    session_ctx=$(inject_session_summary)
    if [ -n "$backup" ]; then
      fallback=$(save_fallback_recipe)
      full="INFIGRAPH CONTEXT RESET: Context was cleared. $session_ctx You MUST print a visible summary of prior session context to the user (summary + pending tasks) so they can see session continuity is working. A pre-clear transcript backup exists at $backup. Read this file (contains recent exchanges as clean markdown), extract key context (summary, decisions, pending tasks, files touched), then persist it: $fallback After saving (via either path), delete the backup file with Bash rm. Do NOT proceed with user work until this recovery is complete."
      escaped=$(printf '%s' "$full" | json_escape)
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"$escaped"}}
ENDJSON
    else
      escaped=$(printf '%s' "$session_ctx" | json_escape)
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"INFIGRAPH CONTEXT RESET: Context was cleared. $escaped You MUST print a visible summary of prior session context to the user (summary + pending tasks) so they can see session continuity is working."}}
ENDJSON
    fi
    ;;
esac

exit 0
