#!/usr/bin/env bash
# Simulates the three index-freshness failure modes from issue #48 against a
# real infigraph-mcp HTTP server: branch switch/new commit, process restart,
# and a dirty working tree. Exits non-zero on the first failed assertion.
set -euo pipefail

REPO=/work/repo
mkdir -p "$REPO"
cd "$REPO"

git init -q
git config user.email "test@example.com"
git config user.name "Freshness Test"
# Infigraph writes its own .infigraph/ (runtime state) and .claude/CLAUDE.md
# (agent instructions) as side effects of indexing — real projects ignore
# both; without this, both scenarios below would misreport "dirty" from
# Infigraph's own bookkeeping rather than the user's actual edits.
cat > .gitignore <<'EOF'
.infigraph/
.claude/
EOF
cat > lib.py <<'EOF'
def helper():
    return 1

def caller():
    return helper()
EOF
git add .
git commit -q -m "initial"

PASS=0
FAIL=0

assert_contains() {
    local haystack="$1" needle="$2" desc="$3"
    if echo "$haystack" | grep -qF -- "$needle"; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc"
        echo "    expected to find: $needle"
        echo "    in: $haystack"
        FAIL=$((FAIL + 1))
    fi
}

assert_not_contains() {
    local haystack="$1" needle="$2" desc="$3"
    if echo "$haystack" | grep -qF -- "$needle"; then
        echo "  FAIL: $desc"
        echo "    did not expect to find: $needle"
        echo "    in: $haystack"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    fi
}

mcp_call() {
    local tool="$1" args_json="$2"
    curl -s -X POST "http://127.0.0.1:8642/tools/mcp" \
        -H 'Content-Type: application/json' \
        -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$tool\",\"arguments\":$args_json}}" \
        | jq -r '.result.content[0].text // .result // empty'
}

# `infigraph index` auto-starts a background watcher holding the graph's
# write lock for its lifetime — stop it after every index so later commands
# (another index, `infigraph watch`, the MCP server) can open the graph.
stop_auto_watcher() {
    infigraph watch-stop >/dev/null 2>&1 || true
    sleep 1
}

echo "=== Scenario setup: initial index ==="
infigraph index --no-embed
stop_auto_watcher
# Symbol IDs are "{file}::{name}" (per `infigraph callers --help`); confirm
# helper() was actually indexed under this ID before relying on it below.
SYMBOL_ID="lib.py::helper"
infigraph symbols lib.py | grep -q "helper" || {
    echo "FAIL: helper() not found in 'infigraph symbols lib.py' output"
    exit 1
}
echo "symbol_id = $SYMBOL_ID"

echo ""
echo "=== Scenario 1: branch switch / new commit (no reindex) ==="
git checkout -q -b other-branch
cat >> lib.py <<'EOF'

def added_on_other_branch():
    pass
EOF
git commit -q -am "change on other branch"

infigraph-mcp --serve --mcp-port=8642 &
MCP_PID=$!
sleep 1

OUT=$(mcp_call "trace_callers" "{\"path\":\"$REPO\",\"symbol_id\":\"$SYMBOL_ID\"}")
assert_contains "$OUT" "stale" "trace_callers warns after branch switch"
assert_contains "$OUT" "indexed_head=" "trace_callers warning includes indexed_head"
assert_contains "$OUT" "current_head=" "trace_callers warning includes current_head"

OUT=$(mcp_call "find_all_references" "{\"path\":\"$REPO\",\"symbol_id\":\"$SYMBOL_ID\"}")
assert_contains "$OUT" "stale" "find_all_references warns after branch switch"

kill "$MCP_PID" 2>/dev/null || true
wait "$MCP_PID" 2>/dev/null || true
# infigraph-mcp is a supervisor/worker pair — the direct PID above is only
# the supervisor; make sure the worker (which actually holds the graph open)
# is gone too before the next scenario tries to open the same graph.
pkill -f infigraph-mcp 2>/dev/null || true
sleep 1

echo ""
echo "=== Reindex to clear staleness before next scenario ==="
infigraph index --no-embed
stop_auto_watcher

echo ""
echo "=== Scenario 2: process restart misses a commit made while down ==="
infigraph watch --debounce 200 > /tmp/watch1.log 2>&1 &
WATCH_PID=$!
sleep 1
kill "$WATCH_PID" 2>/dev/null || true
wait "$WATCH_PID" 2>/dev/null || true
sleep 1

cat >> lib.py <<'EOF'

def added_while_watcher_down():
    pass
EOF
git commit -q -am "change while watcher was down"

infigraph watch --debounce 200 > /tmp/watch2.log 2>&1 &
WATCH_PID=$!
sleep 2
kill "$WATCH_PID" 2>/dev/null || true
wait "$WATCH_PID" 2>/dev/null || true

assert_contains "$(cat /tmp/watch2.log)" "reconcile: lib.py" \
    "watcher restart reconciles the commit made while it was down"

echo ""
echo "=== Reindex to clear staleness before next scenario ==="
infigraph index --no-embed
stop_auto_watcher

echo ""
echo "=== Scenario 3: dirty working tree (no commit) ==="
cat >> lib.py <<'EOF'

def uncommitted_edit():
    pass
EOF
# Deliberately not committed.

infigraph-mcp --serve --mcp-port=8642 &
MCP_PID=$!
sleep 1

OUT=$(mcp_call "trace_callers" "{\"path\":\"$REPO\",\"symbol_id\":\"$SYMBOL_ID\"}")
assert_contains "$OUT" "stale" "trace_callers warns on dirty working tree"
assert_contains "$OUT" "uncommitted changes" "warning specifically names uncommitted changes"

kill "$MCP_PID" 2>/dev/null || true
wait "$MCP_PID" 2>/dev/null || true
# infigraph-mcp is a supervisor/worker pair — the direct PID above is only
# the supervisor; make sure the worker (which actually holds the graph open)
# is gone too before the next scenario tries to open the same graph.
pkill -f infigraph-mcp 2>/dev/null || true
sleep 1

echo ""
echo "=== Results: $PASS passed, $FAIL failed ==="
if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
