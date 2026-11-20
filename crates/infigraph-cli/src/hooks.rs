use anyhow::Result;
use serde_json::json;

pub(crate) const ENFORCE_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# Infigraph PreToolUse enforcement hook — deny-by-default
# Blocks raw search/file tools in Infigraph-indexed projects.
# Deny-by-default. Fallback sentinel allows raw tools after infigraph search returns no results.
#
# MCP-liveness gate: every existing escape hatch below (the search-fallback
# sentinel, the test-context sentinel) only ever gets set AFTER a real
# mcp__infigraph__* tool call has already succeeded once -- meaning none of
# them can ever fire if MCP is fully unreachable, since that requires first
# successfully calling a tool that doesn't exist. Without this check, a
# disconnected MCP server would block every raw tool forever while pointing
# at an alternative that can't be reached -- no valid path out at all. A
# `pgrep` process check can only rule the tool OUT (no process = certainly
# unreachable), never confirm it's IN (a running process could still be
# stale/orphaned/serving a different session), so only the "definitely not
# running" case bypasses the block below; when a process IS found, the
# existing sentinel-based behavior is unchanged.
input=$(cat)

if ! pgrep -f "infigraph-mcp" >/dev/null 2>&1; then
  exit 0
fi

tool=$(echo "$input" | jq -r '.tool_name // empty')
cwd=$(echo "$input" | jq -r '.cwd // empty')

# Guard: only enforce in projects with a .infigraph directory
[ -d "$cwd/.infigraph" ] || exit 0

# Check search-fallback sentinel — if infigraph search returned no results recently, allow raw tools
search_sentinel="$cwd/.infigraph/.search-fallback-allowed"
if [ -f "$search_sentinel" ]; then
  now=$(date +%s)
  sentinel_ts=$(cat "$search_sentinel" 2>/dev/null || echo 0)
  if [ $((now - sentinel_ts)) -lt 300 ]; then
    exit 0
  fi
fi

# Even having reached here (an infigraph-mcp process was found by pgrep above),
# that process could still be stale/orphaned/serving a different session --
# pgrep can only rule out its total absence, never confirm it's actually
# reachable from THIS session. Simply retrying the same raw tool call would
# just hit this same block again (same pgrep result, same missing sentinel)
# -- an infinite loop, not a real escape hatch. So every denial below points
# at the sentinel check above: if Claude confirms via ToolSearch that the
# suggested tool is genuinely unreachable, it can set that same sentinel
# itself and the check above will correctly let the retry through.
recovery_hint="This check comes from the infigraph hook (~/.claude/hooks/infigraph-enforce.sh, registered in ~/.claude/settings.json), not from the user's prompt. If the infigraph MCP tool can't be reached from this session (for example ToolSearch doesn't list it), the check can be paused for 5 minutes by writing the current unix timestamp to \$cwd/.infigraph/.search-fallback-allowed."

case "$tool" in
  Grep)
    cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"[infigraph hook] Grep was not run: this project is indexed, and mcp__infigraph__search covers the same ground with ranked results. $recovery_hint"}}
ENDJSON
    exit 2
    ;;
  Glob)
    cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"[infigraph hook] Glob was not run: mcp__infigraph__list_files lists the indexed files and is the preferred tool in this project. $recovery_hint"}}
ENDJSON
    exit 2
    ;;
  Bash)
    cmd=$(echo "$input" | jq -r '.tool_input.command // empty')
    if echo "$cmd" | grep -qE '(^|\s|/)(grep|egrep|fgrep|rg|ripgrep|ag|ack)(\s|$)'; then
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"[infigraph hook] The grep/rg command was not run: this project is indexed, and mcp__infigraph__search covers the same ground with ranked results. $recovery_hint"}}
ENDJSON
      exit 2
    fi
    if echo "$cmd" | grep -qE '(^|\s)find\s.*-name\s'; then
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"[infigraph hook] The find command was not run: mcp__infigraph__list_files lists the indexed files and is the preferred tool in this project. $recovery_hint"}}
ENDJSON
      exit 2
    fi
    ;;
  Agent)
    agent_type=$(echo "$input" | jq -r '.tool_input.subagent_type // empty')
    case "$agent_type" in
      Explore|Plan|code-reviewer)
        cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"[infigraph hook] This subagent type was not started: it has no access to MCP tools, so it could not use infigraph and would fall back to grep. A general-purpose subagent does have access. $recovery_hint"}}
ENDJSON
        exit 2
        ;;
    esac
    ;;
  Read)
    file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')
    # Allow if offset specified (targeted line-number lookup for Edit)
    has_offset=$(echo "$input" | jq -r '.tool_input.offset // empty')
    if [ -n "$has_offset" ] && [ "$has_offset" != "null" ]; then
      exit 0
    fi
    # Allow if file was recently edited (Edit tracker exemption)
    tracker_file="${TMPDIR:-/tmp}/infigraph-edit-tracker/recent_edits.log"
    if [ -f "$tracker_file" ] && grep -qF "$file_path" "$tracker_file" 2>/dev/null; then
      exit 0
    fi
    # Block — use infigraph tools. If infigraph search returns nothing, sentinel allows retry.
    echo "[infigraph hook] Read was not run: in this project mcp__infigraph__get_doc_context, search or get_code_snippet return the same code with callers and callees. Read is still allowed with an offset (e.g. for Edit line numbers). $recovery_hint" >&2
    exit 2
    ;;
  Write|Edit)
    file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')
    if echo "$file_path" | grep -qE '(test_[^/]+\.[^/]+|[^/]+_test\.[^/]+|[^/]+\.test\.[^/]+|[^/]+_spec\.[^/]+|[^/]+\.spec\.[^/]+|tests/[^/]+\.[^/]+|__tests__/|\.feature$|\.karate$)'; then
      sentinel="$cwd/.infigraph/.test-context-called"
      if [ -f "$sentinel" ]; then
        # Check freshness — allow if sentinel written within last 30 minutes
        now=$(date +%s)
        sentinel_ts=$(cat "$sentinel" 2>/dev/null || echo 0)
        if [ $((now - sentinel_ts)) -lt 1800 ]; then
          exit 0
        fi
      fi
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"[infigraph hook] The test-file write was not run yet: calling mcp__infigraph__generate_test_context first gives the project's test conventions and the code under test. $recovery_hint"}}
ENDJSON
      exit 2
    fi
    ;;
esac

exit 0
"#;

pub(crate) const SESSION_SAVE_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
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
    mcp_hint="An infigraph-mcp process is running, though that doesn't guarantee it is connected to this session."
  else
    mcp_hint="No infigraph-mcp process was detected, so mcp__infigraph__save_session will most likely fail."
  fi

  check_first="Before writing a summary, it is cheap to confirm the tool responds (ToolSearch for mcp__infigraph__save_session, or one call with minimal arguments)."

  fallback_recipe="If the tool is unavailable, the same notes can be saved by writing \$cwd/.infigraph/sessions/session_<YYYY-MM-DD>.json (today's UTC date) directly. It is a JSON object with string fields id ('session_<YYYY-MM-DD>'), name (empty unless named), summary, pending_tasks, decisions, files_touched (comma-separated), constraints, assumptions, blockers, and numeric fields created_at, updated_at, last_accessed (unix epoch seconds) and confidence (float). If today's file already exists, merge into it: append decisions separated by ' | ', union files_touched, replace the other text fields, keep created_at, set confidence to max(0.9, existing), and set updated_at and last_accessed to now. Afterwards, write '0' to \${TMPDIR:-/tmp}/infigraph-sessions/\$session_id.count and '1' to \${TMPDIR:-/tmp}/infigraph-sessions/\$session_id.saved, so the /clear guard knows a save happened."

  full_context="[infigraph hook] Periodic reminder, sent automatically every 10th prompt by the infigraph UserPromptSubmit hook (it is not part of the user's message). Saving session notes is optional but useful: a short summary, pending_tasks and decisions via mcp__infigraph__save_session, with no narrative needed for a periodic save. If you save, a one-line note to the user such as 'Saved session notes.' keeps them informed. $mcp_hint $check_first $fallback_recipe If the save isn't practical right now, skip it and carry on with the user's request."

  json_escaped=$(printf '%s' "$full_context" | python3 -c 'import sys,json; print(json.dumps(sys.stdin.read())[1:-1])')

  cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"$json_escaped"}}
ENDJSON
fi

exit 0
"#;

pub(crate) const SESSION_RESET_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# Infigraph PostToolUse hook — resets session save counter after save_session
# stdin: JSON {tool_name, tool_input, ...}

input=$(cat)
tool=$(echo "$input" | jq -r '.tool_name // empty')

[ "$tool" = "mcp__infigraph__save_session" ] || exit 0

session_id=$(echo "$input" | jq -r '.session_id // empty')
[ -z "$session_id" ] && exit 0

counter_file="${TMPDIR:-/tmp}/infigraph-sessions/$session_id.count"
echo "0" > "$counter_file" 2>/dev/null

# Set saved sentinel for clear-guard (separate from exchange counter)
saved_file="${TMPDIR:-/tmp}/infigraph-sessions/$session_id.saved"
echo "1" > "$saved_file" 2>/dev/null

exit 0
"#;

pub(crate) const SESSION_START_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
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
    hint="An infigraph-mcp process is running, though that doesn't guarantee it is connected to this session."
  else
    hint="No infigraph-mcp process was detected, so mcp__infigraph__save_session will most likely fail."
  fi
  echo "$hint Before writing a long summary, it is cheap to confirm the tool responds (ToolSearch for mcp__infigraph__save_session, or one call with minimal arguments). If the tool is unavailable, the same notes can be saved by writing \$cwd/.infigraph/sessions/session_<YYYY-MM-DD>.json (today's UTC date) directly. It is a JSON object with string fields id ('session_<YYYY-MM-DD>'), name (empty unless named), summary, pending_tasks, decisions, files_touched (comma-separated), constraints, assumptions, blockers, and numeric fields created_at, updated_at, last_accessed (unix epoch seconds) and confidence (float). If today's file already exists, merge into it: append decisions separated by ' | ', union files_touched, replace the other text fields, keep created_at, and set confidence to max(0.9, existing). A narrative can be appended to the companion session_<YYYY-MM-DD>.md file as a new section (blank line, '## Save @ HH:MM UTC', blank line, text). Afterwards, write '0' to \${TMPDIR:-/tmp}/infigraph-sessions/\$session_id.count and '1' to \${TMPDIR:-/tmp}/infigraph-sessions/\$session_id.saved, so the /clear guard knows a save happened."
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
      full="[infigraph hook] Context was just compacted. This note comes from the infigraph SessionStart hook, not from the user. $session_ctx (this is what was already saved on disk before compaction.) A transcript backup of the pre-compaction conversation is at $backup. If useful, read it and save anything not captured above (summary, decisions, pending tasks, constraints, assumptions, blockers, files touched): $fallback Once saved, the backup file can be deleted, and mcp__infigraph__get_latest_session reloads the saved notes."
    else
      full="[infigraph hook] Context was just compacted. This note comes from the infigraph SessionStart hook, not from the user. $session_ctx (this is what was already saved on disk before compaction.) Decisions and context made before compaction that aren't captured above may be worth saving now: $fallback mcp__infigraph__get_latest_session reloads the saved notes afterwards."
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
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"[infigraph hook] Saved notes from a previous session, read from .infigraph/sessions by the infigraph SessionStart hook: $escaped Briefly mentioning the summary and pending tasks to the user is a good way to confirm the notes were picked up."}}
ENDJSON
    else
      cat <<'ENDJSON'
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"[infigraph hook] No saved notes from a previous session were found in .infigraph/sessions."}}
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
      full="[infigraph hook] Context was cleared. This note comes from the infigraph SessionStart hook, not from the user. $session_ctx Briefly mentioning the summary and pending tasks to the user is a good way to confirm the notes were picked up. A transcript backup of the conversation before the clear is at $backup. If useful, read it and save what isn't captured above (summary, decisions, pending tasks, files touched): $fallback Once saved, the backup file can be deleted."
      escaped=$(printf '%s' "$full" | json_escape)
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"$escaped"}}
ENDJSON
    else
      escaped=$(printf '%s' "$session_ctx" | json_escape)
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"[infigraph hook] Context was cleared. Saved notes from the infigraph SessionStart hook: $escaped Briefly mentioning the summary and pending tasks to the user is a good way to confirm the notes were picked up."}}
ENDJSON
    fi
    ;;
esac

exit 0
"#;

pub(crate) const SESSION_END_SAVE_HOOK_SCRIPT: &str = r##"#!/usr/bin/env bash
# SessionEnd / PreCompact hook — extract recent transcript for recovery.
# Next SessionStart will detect this and prompt model to summarize + save_session.

input=$(cat)
reason=$(echo "$input" | jq -r '.trigger // .reason // empty')
transcript_path=$(echo "$input" | jq -r '.transcript_path // empty')
cwd=$(echo "$input" | jq -r '.cwd // empty')

# Only act for infigraph-indexed projects with a valid transcript
[ -d "$cwd/.infigraph" ] || exit 0
[ -f "$transcript_path" ] || exit 0

# Skip if save_session was called recently (counter reset to 0 means save just happened)
session_id=$(echo "$input" | jq -r '.session_id // empty')
if [ -n "$session_id" ]; then
  counter_file="${TMPDIR:-/tmp}/infigraph-sessions/$session_id.count"
  if [ -f "$counter_file" ]; then
    count=$(cat "$counter_file" 2>/dev/null || echo 0)
    [ "$count" -eq 0 ] && exit 0
  fi
fi

sessions_dir="$cwd/.infigraph/sessions"
mkdir -p "$sessions_dir"

backup="$sessions_dir/unsaved-transcript-${reason:-unknown}.md"

python3 -c "
import json, sys

messages = []
with open('$transcript_path') as f:
    for line in f:
        try:
            d = json.loads(line)
        except:
            continue
        if d.get('type') not in ('user', 'assistant'):
            continue
        role = d['type']
        msg = d.get('message', {})
        if role == 'user':
            content = msg.get('content', '')
            if isinstance(content, list):
                content = ' '.join(p.get('text','') for p in content if p.get('type')=='text')
            if content.strip():
                messages.append(('user', content.strip()))
        elif role == 'assistant':
            content = msg.get('content', '')
            if isinstance(content, list):
                parts = [p.get('text','') for p in content if p.get('type')=='text']
                content = ' '.join(parts)
            if content.strip():
                messages.append(('assistant', content.strip()))

recent = messages[-40:]
with open('$backup', 'w') as out:
    out.write('# Unsaved session context (last ~20 exchanges)\n\n')
    for role, text in recent:
        out.write(f'## {role.title()}\n{text}\n\n')
" 2>/dev/null

exit 0
"##;

pub(crate) const CLEAR_SUGGEST_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
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
{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"[infigraph hook] Periodic reminder, sent automatically every 5th prompt by the infigraph UserPromptSubmit hook (it is not part of the user's message). If this conversation has grown long, it may help to suggest to the user that they save the session and run /clear to reset context. Otherwise this can be ignored."}}
ENDJSON
fi

exit 0
"#;

pub(crate) const CLEAR_GUARD_HOOK_SCRIPT: &str = r##"#!/usr/bin/env bash
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
"##;

pub fn allowed_tools() -> Vec<String> {
    infigraph_mcp::allowed_tools_from_names()
}

pub(crate) fn install_enforcement_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("infigraph-enforce.sh");
    std::fs::write(&hook_path, ENFORCE_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("  Installed enforcement hook: {}", hook_path.display());

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let hook_entry = json!({
        "matcher": "Grep|Glob|Bash|Read|Write|Edit|Agent",
        "hooks": [{
            "type": "command",
            "command": hook_path.to_string_lossy(),
            "timeout": 5
        }]
    });

    let pre_tool = settings["hooks"]
        .get("PreToolUse")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let already_exists = pre_tool.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-enforce"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !already_exists {
        let mut arr = pre_tool;
        arr.push(hook_entry);
        settings["hooks"]["PreToolUse"] = serde_json::Value::Array(arr);

        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!("  Added PreToolUse hook to {}", settings_path.display());
    } else {
        let expected_matcher = "Grep|Glob|Bash|Read|Write|Edit|Agent";
        let mut arr = pre_tool;
        let mut updated = false;
        for entry in arr.iter_mut() {
            let is_infigraph = entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .map(|hooks| {
                    hooks.iter().any(|h| {
                        h.get("command")
                            .and_then(|c| c.as_str())
                            .map(|c| c.contains("infigraph-enforce"))
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            if is_infigraph {
                let current_matcher = entry.get("matcher").and_then(|m| m.as_str()).unwrap_or("");
                if current_matcher != expected_matcher {
                    entry["matcher"] = json!(expected_matcher);
                    updated = true;
                }
            }
        }
        if updated {
            settings["hooks"]["PreToolUse"] = serde_json::Value::Array(arr);
            let pretty = serde_json::to_string_pretty(&settings)?;
            std::fs::write(&settings_path, pretty)?;
            println!(
                "  Updated PreToolUse matcher in {}",
                settings_path.display()
            );
        } else {
            println!(
                "  PreToolUse hook already configured in {}",
                settings_path.display()
            );
        }
    }

    Ok(())
}

pub(crate) const TEST_CONTEXT_SENTINEL_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# PostToolUse hook: writes sentinel after generate_test_context succeeds.
# Allows Write/Edit enforcement hook to pass for test files.

input=$(cat)
tool=$(echo "$input" | jq -r '.tool_name // empty')
cwd=$(echo "$input" | jq -r '.cwd // empty')

[ "$tool" = "mcp__infigraph__generate_test_context" ] || exit 0
[ -d "$cwd/.infigraph" ] || exit 0

echo "$(date +%s)" > "$cwd/.infigraph/.test-context-called"

exit 0
"#;

pub(crate) const SEARCH_FALLBACK_SENTINEL_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# PostToolUse hook: writes fallback sentinel when infigraph search returns no results.
# Allows Grep/Glob/Read enforcement to pass as fallback.

input=$(cat)
tool=$(echo "$input" | jq -r '.tool_name // empty')
cwd=$(echo "$input" | jq -r '.cwd // empty')

[ -d "$cwd/.infigraph" ] || exit 0

case "$tool" in
  mcp__infigraph__search|mcp__infigraph__search_code|mcp__infigraph__search_symbols|mcp__infigraph__list_files)
    output=$(echo "$input" | jq -r '.tool_output // empty')
    # Check for empty results indicators
    if echo "$output" | grep -qE '(0 symbol results, 0 text matches|0 results|No files found|No symbols found|No matches)'; then
      mkdir -p "$cwd/.infigraph" 2>/dev/null
      echo "$(date +%s)" > "$cwd/.infigraph/.search-fallback-allowed"
    fi
    ;;
esac

exit 0
"#;

pub(crate) const EDIT_TRACKER_HOOK_SCRIPT: &str = r#"#!/usr/bin/env bash
# PostToolUse hook for Edit: records file path to allow subsequent Read for line numbers.
# Tracks files that were recently edited so the Read enforcement hook can exempt them.

input=$(cat)
tool=$(echo "$input" | jq -r '.tool_name // empty')

[ "$tool" = "Edit" ] || exit 0

file_path=$(echo "$input" | jq -r '.tool_input.file_path // empty')
[ -z "$file_path" ] && exit 0

tracker_dir="${TMPDIR:-/tmp}/infigraph-edit-tracker"
mkdir -p "$tracker_dir" 2>/dev/null

# Write file path with timestamp — Read hook checks recency
echo "$(date +%s) $file_path" >> "$tracker_dir/recent_edits.log"

# Prune entries older than 5 minutes
now=$(date +%s)
if [ -f "$tracker_dir/recent_edits.log" ]; then
  awk -v cutoff=$((now - 300)) '$1 >= cutoff' "$tracker_dir/recent_edits.log" > "$tracker_dir/recent_edits.tmp" 2>/dev/null
  mv "$tracker_dir/recent_edits.tmp" "$tracker_dir/recent_edits.log" 2>/dev/null
fi

exit 0
"#;

pub(crate) fn install_edit_tracker_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("infigraph-edit-tracker.sh");
    std::fs::write(&hook_path, EDIT_TRACKER_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("  Installed edit tracker hook: {}", hook_path.display());

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let post_tool = settings["hooks"]
        .get("PostToolUse")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Check if edit tracker already registered in any PostToolUse entry
    let already_exists = post_tool.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-edit-tracker"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !already_exists {
        // Find existing Edit|Write matcher entry and add our hook, or create new entry
        let mut arr = post_tool;
        let mut added = false;
        for entry in arr.iter_mut() {
            let matcher = entry.get("matcher").and_then(|m| m.as_str()).unwrap_or("");
            if matcher.contains("Edit") {
                if let Some(hooks) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                    hooks.push(json!({
                        "type": "command",
                        "command": hook_path.to_string_lossy(),
                        "timeout": 5,
                        "async": true
                    }));
                    added = true;
                    break;
                }
            }
        }
        if !added {
            arr.push(json!({
                "matcher": "Edit|Write|NotebookEdit",
                "hooks": [{
                    "type": "command",
                    "command": hook_path.to_string_lossy(),
                    "timeout": 5,
                    "async": true
                }]
            }));
        }
        settings["hooks"]["PostToolUse"] = serde_json::Value::Array(arr);

        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!(
            "  Added PostToolUse edit tracker hook to {}",
            settings_path.display()
        );
    } else {
        println!(
            "  Edit tracker hook already configured in {}",
            settings_path.display()
        );
    }

    Ok(())
}

pub(crate) fn install_session_save_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let save_hook_path = hooks_dir.join("infigraph-session-save.sh");
    std::fs::write(&save_hook_path, SESSION_SAVE_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&save_hook_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let reset_hook_path = hooks_dir.join("infigraph-session-reset.sh");
    std::fs::write(&reset_hook_path, SESSION_RESET_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&reset_hook_path, std::fs::Permissions::from_mode(0o755))?;
    }

    let start_hook_path = hooks_dir.join("infigraph-session-start.sh");
    std::fs::write(&start_hook_path, SESSION_START_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&start_hook_path, std::fs::Permissions::from_mode(0o755))?;
    }

    println!(
        "  Installed session save hook: {}",
        save_hook_path.display()
    );
    println!(
        "  Installed session reset hook: {}",
        reset_hook_path.display()
    );
    println!(
        "  Installed session start hook: {}",
        start_hook_path.display()
    );

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    // UserPromptSubmit hook for session save reminder
    let save_hook_entry = json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": save_hook_path.to_string_lossy(),
            "timeout": 5
        }]
    });

    let user_prompt = settings["hooks"]
        .get("UserPromptSubmit")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let save_exists = user_prompt.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-session-save"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    let mut settings_changed = false;

    if !save_exists {
        let mut arr = user_prompt;
        arr.push(save_hook_entry);
        settings["hooks"]["UserPromptSubmit"] = serde_json::Value::Array(arr);
        settings_changed = true;
        println!(
            "  Added UserPromptSubmit hook to {}",
            settings_path.display()
        );
    } else {
        println!("  UserPromptSubmit session hook already configured");
    }

    // PostToolUse hook for counter reset
    let reset_hook_entry = json!({
        "matcher": "mcp__infigraph__save_session",
        "hooks": [{
            "type": "command",
            "command": reset_hook_path.to_string_lossy(),
            "timeout": 5,
            "async": true
        }]
    });

    let post_tool = settings["hooks"]
        .get("PostToolUse")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let reset_exists = post_tool.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-session-reset"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !reset_exists {
        let mut arr = post_tool;
        arr.push(reset_hook_entry);
        settings["hooks"]["PostToolUse"] = serde_json::Value::Array(arr);
        settings_changed = true;
        println!(
            "  Added PostToolUse reset hook to {}",
            settings_path.display()
        );
    } else {
        println!("  PostToolUse session reset hook already configured");
    }

    // SessionStart hook for compaction save + startup/resume restore
    let start_hook_entry = json!({
        "hooks": [{
            "type": "command",
            "command": start_hook_path.to_string_lossy(),
            "timeout": 5
        }]
    });

    let session_start = settings["hooks"]
        .get("SessionStart")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let start_exists = session_start.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-session-start"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !start_exists {
        let mut arr = session_start;
        arr.push(start_hook_entry);
        settings["hooks"]["SessionStart"] = serde_json::Value::Array(arr);
        settings_changed = true;
        println!("  Added SessionStart hook to {}", settings_path.display());
    } else {
        println!("  SessionStart session hook already configured");
    }

    if settings_changed {
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
    }

    Ok(())
}

pub(crate) fn install_clear_suggest_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("infigraph-clear-suggest.sh");
    std::fs::write(&hook_path, CLEAR_SUGGEST_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("  Installed clear suggest hook: {}", hook_path.display());

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let hook_entry = json!({
        "hooks": [{
            "type": "command",
            "command": hook_path.to_string_lossy(),
            "timeout": 5
        }]
    });

    let user_prompt = settings["hooks"]
        .get("UserPromptSubmit")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let already_exists = user_prompt.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-clear-suggest"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !already_exists {
        let mut arr = user_prompt;
        arr.push(hook_entry);
        settings["hooks"]["UserPromptSubmit"] = serde_json::Value::Array(arr);
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!(
            "  Added UserPromptSubmit clear-suggest hook to {}",
            settings_path.display()
        );
    } else {
        println!("  UserPromptSubmit clear-suggest hook already configured");
    }

    Ok(())
}

pub(crate) fn install_clear_guard_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("infigraph-clear-guard.sh");
    std::fs::write(&hook_path, CLEAR_GUARD_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("  Installed clear guard hook: {}", hook_path.display());

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let hook_entry = json!({
        "hooks": [{
            "type": "command",
            "command": hook_path.to_string_lossy(),
            "timeout": 5
        }]
    });

    let user_prompt = settings["hooks"]
        .get("UserPromptSubmit")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let already_exists = user_prompt.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-clear-guard"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !already_exists {
        let mut arr = user_prompt;
        arr.push(hook_entry);
        settings["hooks"]["UserPromptSubmit"] = serde_json::Value::Array(arr);
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!(
            "  Added UserPromptSubmit clear-guard hook to {}",
            settings_path.display()
        );
    } else {
        println!("  UserPromptSubmit clear-guard hook already configured");
    }

    Ok(())
}

pub(crate) fn install_session_end_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("infigraph-session-end-save.sh");
    std::fs::write(&hook_path, SESSION_END_SAVE_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    println!("  Installed session-end save hook: {}", hook_path.display());

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let hook_entry = json!({
        "hooks": [{
            "type": "command",
            "command": hook_path.to_string_lossy(),
            "timeout": 10
        }]
    });

    let session_end = settings["hooks"]
        .get("SessionEnd")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let already_exists = session_end.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-session-end"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !already_exists {
        let mut arr = session_end;
        arr.push(hook_entry.clone());
        settings["hooks"]["SessionEnd"] = serde_json::Value::Array(arr);
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!("  Added SessionEnd hook to {}", settings_path.display());
    } else {
        println!("  SessionEnd session-end hook already configured");
    }

    // Also register for PreCompact — the same script handles both events
    // (reads .trigger for PreCompact, .reason for SessionEnd), so pre-compaction
    // context gets the same transcript-backup safety net as session-end does.
    let pre_compact = settings["hooks"]
        .get("PreCompact")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let pre_compact_exists = pre_compact.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-session-end"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !pre_compact_exists {
        let mut arr = pre_compact;
        arr.push(hook_entry);
        settings["hooks"]["PreCompact"] = serde_json::Value::Array(arr);
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!("  Added PreCompact hook to {}", settings_path.display());
    } else {
        println!("  PreCompact session-end hook already configured");
    }

    Ok(())
}

pub(crate) fn install_claude_allowlist(home: &std::path::Path) -> Result<()> {
    let settings_path = home.join(".claude").join("settings.local.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content).unwrap_or(json!({}))
    } else {
        json!({})
    };

    if settings.get("permissions").is_none() {
        settings["permissions"] = json!({});
    }
    let existing: Vec<String> = settings["permissions"]
        .get("allow")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let existing_set: std::collections::HashSet<&str> =
        existing.iter().map(|s| s.as_str()).collect();
    let mut allow_list = existing.clone();
    let mut added = 0usize;
    for tool in allowed_tools() {
        if !existing_set.contains(tool.as_str()) {
            allow_list.push(tool);
            added += 1;
        }
    }

    if added > 0 {
        settings["permissions"]["allow"] = serde_json::Value::Array(
            allow_list
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );
        if let Some(parent) = settings_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!(
            "  Added {} Infigraph MCP tools to Claude Code allowlist ({})",
            added,
            settings_path.display()
        );
    } else {
        println!(
            "  Claude Code allowlist already up to date ({})",
            settings_path.display()
        );
    }

    Ok(())
}

pub(crate) fn uninstall_claude_allowlist(home: &std::path::Path) -> Result<()> {
    let settings_path = home.join(".claude").join("settings.local.json");
    if !settings_path.is_file() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&settings_path)?;
    let mut settings: serde_json::Value = serde_json::from_str(&content).unwrap_or(json!({}));

    let existing: Vec<String> = settings["permissions"]
        .get("allow")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let infigraph_tools = allowed_tools();
    let infigraph_set: std::collections::HashSet<&str> =
        infigraph_tools.iter().map(|s| s.as_str()).collect();
    let filtered: Vec<String> = existing
        .into_iter()
        .filter(|s| !infigraph_set.contains(s.as_str()))
        .collect();
    let removed = filtered.len()
        < settings["permissions"]["allow"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);

    if removed {
        settings["permissions"]["allow"] = serde_json::Value::Array(
            filtered
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        );
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!(
            "  Removed Infigraph MCP tools from Claude Code allowlist ({})",
            settings_path.display()
        );
    }

    Ok(())
}

pub(crate) fn install_test_context_sentinel_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("infigraph-test-context-sentinel.sh");
    std::fs::write(&hook_path, TEST_CONTEXT_SENTINEL_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    println!(
        "  Installed test-context sentinel hook: {}",
        hook_path.display()
    );

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let post_tool = settings["hooks"]
        .get("PostToolUse")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let already_exists = post_tool.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-test-context-sentinel"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !already_exists {
        let mut arr = post_tool;
        arr.push(json!({
            "matcher": "mcp__infigraph__generate_test_context",
            "hooks": [{
                "type": "command",
                "command": hook_path.to_string_lossy(),
                "timeout": 5,
                "async": true
            }]
        }));
        settings["hooks"]["PostToolUse"] = serde_json::Value::Array(arr);
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!(
            "  Added PostToolUse test-context sentinel hook to {}",
            settings_path.display()
        );
    } else {
        println!("  Test-context sentinel hook already configured");
    }

    Ok(())
}

pub(crate) fn install_search_fallback_sentinel_hook(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    let hook_path = hooks_dir.join("infigraph-search-fallback-sentinel.sh");
    std::fs::write(&hook_path, SEARCH_FALLBACK_SENTINEL_HOOK_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))?;
    }
    println!(
        "  Installed search-fallback sentinel hook: {}",
        hook_path.display()
    );

    let settings_path = home.join(".claude").join("settings.json");
    let mut settings: serde_json::Value = if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        serde_json::from_str(&content)?
    } else {
        json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = json!({});
    }

    let post_tool = settings["hooks"]
        .get("PostToolUse")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let already_exists = post_tool.iter().any(|entry| {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("infigraph-search-fallback-sentinel"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    });

    if !already_exists {
        let mut arr = post_tool;
        arr.push(json!({
            "matcher": "mcp__infigraph__search|mcp__infigraph__search_code|mcp__infigraph__search_symbols|mcp__infigraph__list_files",
            "hooks": [{
                "type": "command",
                "command": hook_path.to_string_lossy(),
                "timeout": 5,
                "async": true
            }]
        }));
        settings["hooks"]["PostToolUse"] = serde_json::Value::Array(arr);
        let pretty = serde_json::to_string_pretty(&settings)?;
        std::fs::write(&settings_path, pretty)?;
        println!(
            "  Added PostToolUse search-fallback sentinel hook to {}",
            settings_path.display()
        );
    } else {
        println!("  Search-fallback sentinel hook already configured");
    }

    Ok(())
}

pub(crate) fn uninstall_hooks(home: &std::path::Path) -> Result<()> {
    let hooks_dir = home.join(".claude").join("hooks");
    for hook_file in &[
        "infigraph-enforce.sh",
        "infigraph-edit-tracker.sh",
        "infigraph-session-save.sh",
        "infigraph-session-reset.sh",
        "infigraph-session-start.sh",
        "infigraph-clear-suggest.sh",
        "infigraph-clear-guard.sh",
        "infigraph-session-end-save.sh",
        "infigraph-test-context-sentinel.sh",
        "infigraph-search-fallback-sentinel.sh",
    ] {
        let hook_path = hooks_dir.join(hook_file);
        if hook_path.exists() {
            std::fs::remove_file(&hook_path)?;
            println!("  Removed hook: {}", hook_path.display());
        }
    }

    let settings_path = home.join(".claude").join("settings.json");
    if settings_path.is_file() {
        let content = std::fs::read_to_string(&settings_path)?;
        if let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(&content) {
            let infigraph_hook = |entry: &serde_json::Value| -> bool {
                entry
                    .get("hooks")
                    .and_then(|h| h.as_array())
                    .map(|hooks| {
                        hooks.iter().any(|h| {
                            h.get("command")
                                .and_then(|c| c.as_str())
                                .map(|c| c.contains("infigraph-"))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
            };
            for event in &[
                "PreToolUse",
                "UserPromptSubmit",
                "PostToolUse",
                "SessionStart",
                "SessionEnd",
                "PreCompact",
            ] {
                if let Some(arr) = settings["hooks"]
                    .get_mut(*event)
                    .and_then(|v| v.as_array_mut())
                {
                    let before = arr.len();
                    arr.retain(|entry| !infigraph_hook(entry));
                    if arr.len() < before {
                        println!(
                            "  Removed {} hook(s) from {}",
                            event,
                            settings_path.display()
                        );
                    }
                }
            }
            let pretty = serde_json::to_string_pretty(&settings)?;
            std::fs::write(&settings_path, pretty)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_home() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        std::fs::create_dir_all(home.join(".claude/hooks")).unwrap();
        std::fs::write(home.join(".claude/settings.json"), "{}").unwrap();
        (tmp, home)
    }

    #[test]
    fn install_enforcement_hook_creates_file_and_settings() {
        let (_tmp, home) = setup_home();
        install_enforcement_hook(&home).unwrap();

        let hook_path = home.join(".claude/hooks/infigraph-enforce.sh");
        assert!(hook_path.exists());

        let content = std::fs::read_to_string(&hook_path).unwrap();
        assert!(content.contains("deny-by-default"));
        assert!(content.contains("search-fallback-allowed"));

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let pre_tool = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 1);
        assert_eq!(
            pre_tool[0]["matcher"].as_str().unwrap(),
            "Grep|Glob|Bash|Read|Write|Edit|Agent"
        );
    }

    #[test]
    fn install_enforcement_hook_idempotent() {
        let (_tmp, home) = setup_home();
        install_enforcement_hook(&home).unwrap();
        install_enforcement_hook(&home).unwrap();

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let pre_tool = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre_tool.len(), 1);
    }

    #[test]
    fn install_edit_tracker_hook_creates_file() {
        let (_tmp, home) = setup_home();
        install_edit_tracker_hook(&home).unwrap();

        assert!(home
            .join(".claude/hooks/infigraph-edit-tracker.sh")
            .exists());

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let post_tool = settings["hooks"]["PostToolUse"].as_array().unwrap();
        assert!(post_tool.iter().any(|e| {
            e["hooks"].as_array().unwrap().iter().any(|h| {
                h["command"]
                    .as_str()
                    .unwrap()
                    .contains("infigraph-edit-tracker")
            })
        }));
    }

    #[test]
    fn install_test_context_sentinel() {
        let (_tmp, home) = setup_home();
        install_test_context_sentinel_hook(&home).unwrap();

        assert!(home
            .join(".claude/hooks/infigraph-test-context-sentinel.sh")
            .exists());

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let post_tool = settings["hooks"]["PostToolUse"].as_array().unwrap();
        let entry = post_tool
            .iter()
            .find(|e| {
                e["matcher"]
                    .as_str()
                    .map(|m| m.contains("generate_test_context"))
                    .unwrap_or(false)
            })
            .expect("should have test-context matcher");
        assert!(entry["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("test-context-sentinel"));
    }

    #[test]
    fn install_search_fallback_sentinel() {
        let (_tmp, home) = setup_home();
        install_search_fallback_sentinel_hook(&home).unwrap();

        assert!(home
            .join(".claude/hooks/infigraph-search-fallback-sentinel.sh")
            .exists());

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let post_tool = settings["hooks"]["PostToolUse"].as_array().unwrap();
        let entry = post_tool
            .iter()
            .find(|e| {
                e["matcher"]
                    .as_str()
                    .map(|m| m.contains("mcp__infigraph__search"))
                    .unwrap_or(false)
            })
            .expect("should have search fallback matcher");
        assert!(entry["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("search-fallback-sentinel"));
    }

    #[test]
    fn install_session_hooks() {
        let (_tmp, home) = setup_home();
        install_session_save_hook(&home).unwrap();

        assert!(home
            .join(".claude/hooks/infigraph-session-save.sh")
            .exists());
        assert!(home
            .join(".claude/hooks/infigraph-session-reset.sh")
            .exists());
        assert!(home
            .join(".claude/hooks/infigraph-session-start.sh")
            .exists());

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(!settings["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(!settings["hooks"]["PostToolUse"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(!settings["hooks"]["SessionStart"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn install_session_end_hook_creates_file() {
        let (_tmp, home) = setup_home();
        install_session_end_hook(&home).unwrap();

        assert!(home
            .join(".claude/hooks/infigraph-session-end-save.sh")
            .exists());

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(!settings["hooks"]["SessionEnd"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(
            !settings["hooks"]["PreCompact"]
                .as_array()
                .unwrap()
                .is_empty(),
            "PreCompact should get the same transcript-backup hook as SessionEnd, \
             so pre-compaction context isn't lost the same way pre-session-end context is"
        );
    }

    #[test]
    fn install_clear_suggest_hook_creates_file() {
        let (_tmp, home) = setup_home();
        install_clear_suggest_hook(&home).unwrap();

        assert!(home
            .join(".claude/hooks/infigraph-clear-suggest.sh")
            .exists());
    }

    #[test]
    fn install_clear_guard_hook_creates_file() {
        let (_tmp, home) = setup_home();
        install_clear_guard_hook(&home).unwrap();

        assert!(home.join(".claude/hooks/infigraph-clear-guard.sh").exists());
    }

    #[test]
    fn uninstall_removes_all_hook_files() {
        let (_tmp, home) = setup_home();

        install_enforcement_hook(&home).unwrap();
        install_edit_tracker_hook(&home).unwrap();
        install_session_save_hook(&home).unwrap();
        install_clear_suggest_hook(&home).unwrap();
        install_clear_guard_hook(&home).unwrap();
        install_session_end_hook(&home).unwrap();
        install_test_context_sentinel_hook(&home).unwrap();
        install_search_fallback_sentinel_hook(&home).unwrap();

        let hook_files = [
            "infigraph-enforce.sh",
            "infigraph-edit-tracker.sh",
            "infigraph-session-save.sh",
            "infigraph-session-reset.sh",
            "infigraph-session-start.sh",
            "infigraph-clear-suggest.sh",
            "infigraph-clear-guard.sh",
            "infigraph-session-end-save.sh",
            "infigraph-test-context-sentinel.sh",
            "infigraph-search-fallback-sentinel.sh",
        ];
        for f in &hook_files {
            assert!(
                home.join(".claude/hooks").join(f).exists(),
                "{f} should exist after install"
            );
        }

        uninstall_hooks(&home).unwrap();

        for f in &hook_files {
            assert!(
                !home.join(".claude/hooks").join(f).exists(),
                "{f} should be removed"
            );
        }
    }

    #[test]
    fn uninstall_cleans_settings_json() {
        let (_tmp, home) = setup_home();

        install_enforcement_hook(&home).unwrap();
        install_edit_tracker_hook(&home).unwrap();
        install_session_save_hook(&home).unwrap();
        install_clear_guard_hook(&home).unwrap();
        install_session_end_hook(&home).unwrap();
        install_test_context_sentinel_hook(&home).unwrap();
        install_search_fallback_sentinel_hook(&home).unwrap();

        uninstall_hooks(&home).unwrap();

        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();

        for event in &[
            "PreToolUse",
            "PostToolUse",
            "UserPromptSubmit",
            "SessionStart",
            "SessionEnd",
            "PreCompact",
        ] {
            let count = settings["hooks"][event]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            assert_eq!(count, 0, "{event} should be empty after uninstall");
        }
    }

    #[test]
    fn enforce_script_covers_all_tool_cases() {
        let script = ENFORCE_HOOK_SCRIPT;
        assert!(script.contains("Grep)"));
        assert!(script.contains("Glob)"));
        assert!(script.contains("Bash)"));
        assert!(script.contains("Agent)"));
        assert!(script.contains("Read)"));
        assert!(script.contains("Write|Edit)"));
        assert!(script.contains("search-fallback-allowed"));
        assert!(script.contains("test-context-called"));
    }

    #[test]
    fn search_fallback_sentinel_covers_all_search_tools() {
        let script = SEARCH_FALLBACK_SENTINEL_HOOK_SCRIPT;
        assert!(script.contains("mcp__infigraph__search|"));
        assert!(script.contains("mcp__infigraph__search_code|"));
        assert!(script.contains("mcp__infigraph__search_symbols|"));
        assert!(script.contains("mcp__infigraph__list_files"));
    }

    #[test]
    fn session_start_resets_sentinels_on_clear() {
        let script = SESSION_START_HOOK_SCRIPT;
        assert!(script.contains("rm -f \"$cwd/.infigraph/.test-context-called\""));
        assert!(script.contains("rm -f \"$cwd/.infigraph/.search-fallback-allowed\""));
    }
}
