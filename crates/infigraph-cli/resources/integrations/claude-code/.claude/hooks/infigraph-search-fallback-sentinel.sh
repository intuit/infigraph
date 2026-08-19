#!/usr/bin/env bash
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
