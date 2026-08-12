#!/usr/bin/env bash
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
recovery_hint="If a quick ToolSearch shows the suggested infigraph tool genuinely isn't available despite this hook's process check, write the current unix timestamp to \$cwd/.infigraph/.search-fallback-allowed (the same sentinel checked above) and retry -- it will be allowed then."

case "$tool" in
  Grep)
    cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"BLOCKED: Use mcp__infigraph__search instead of Grep. $recovery_hint"}}
ENDJSON
    exit 2
    ;;
  Glob)
    cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"BLOCKED: Use mcp__infigraph__list_files instead of Glob. $recovery_hint"}}
ENDJSON
    exit 2
    ;;
  Bash)
    cmd=$(echo "$input" | jq -r '.tool_input.command // empty')
    # Only flag grep/rg-family tools when NOT immediately preceded by a pipe --
    # `cmd 2>&1 | grep -iE "error"` filters another command's output (allowed,
    # matches this repo's own CLAUDE.md guidance); a bare/leading grep call is
    # a code search and should go through mcp__infigraph__search instead.
    # Note: intentionally avoids \b (unsupported by macOS's BSD sed) in favor
    # of an explicit [[:space:]]/end-of-string bound -- verified against both
    # GNU and BSD sed during this fix.
    cmd_without_piped_grep=$(echo "$cmd" | sed -E 's/\|[[:space:]]*(grep|egrep|fgrep|rg|ripgrep|ag|ack)([[:space:]]|$)[^|]*/|/g')
    if echo "$cmd_without_piped_grep" | grep -qE '(^|\s|/)(grep|egrep|fgrep|rg|ripgrep|ag|ack)(\s|$)'; then
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"BLOCKED: Use mcp__infigraph__search instead of grep/rg. $recovery_hint"}}
ENDJSON
      exit 2
    fi
    if echo "$cmd" | grep -qE '(^|\s)find\s.*-name\s'; then
      cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"BLOCKED: Use mcp__infigraph__list_files instead of find. $recovery_hint"}}
ENDJSON
      exit 2
    fi
    ;;
  Agent)
    agent_type=$(echo "$input" | jq -r '.tool_input.subagent_type // empty')
    case "$agent_type" in
      Explore|Plan|code-reviewer)
        cat <<ENDJSON
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"BLOCKED: This agent type lacks MCP access. Use general-purpose agent instead. $recovery_hint"}}
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
    echo "BLOCKED: Use mcp__infigraph__get_doc_context, search, or get_code_snippet. Read only for Edit line numbers (pass offset). $recovery_hint" >&2
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
{"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"BLOCKED: Call mcp__infigraph__generate_test_context before writing tests. $recovery_hint"}}
ENDJSON
      exit 2
    fi
    ;;
esac

exit 0
