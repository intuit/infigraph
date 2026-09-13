#!/usr/bin/env bash
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
