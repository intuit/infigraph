#!/usr/bin/env bash
# PostToolUse hook: writes sentinel after generate_test_context succeeds.
# Allows Write/Edit enforcement hook to pass for test files.

input=$(cat)
tool=$(echo "$input" | jq -r '.tool_name // empty')
cwd=$(echo "$input" | jq -r '.cwd // empty')

[ "$tool" = "mcp__infigraph__generate_test_context" ] || exit 0
[ -d "$cwd/.infigraph" ] || exit 0

echo "$(date +%s)" > "$cwd/.infigraph/.test-context-called"

exit 0
