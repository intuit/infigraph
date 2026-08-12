#!/usr/bin/env bash
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
