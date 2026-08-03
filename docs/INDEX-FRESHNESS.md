# Index Freshness

Lets graph-backed MCP tools tell you when their answer might not match your current code, instead of silently answering from a stale graph.

## In plain terms

Infigraph keeps a persistent graph of your codebase, and a background file watcher normally keeps it in sync as you edit. But the watcher only reacts to one thing: a file changing on disk. Three common situations slip past that:

- **You switch git branches or rebase.** Dozens of files can change at once; nothing tells the watcher "this was a branch switch, treat it as a bigger deal."
- **The watcher's process restarts** (crash, redeploy, laptop sleep/wake). It has no memory of what happened to the repo while it was down — commits made during that gap were previously never noticed.
- **You have uncommitted edits.** The graph reflects the last indexed commit, not what's currently in your editor.

Before this change, none of that was visible in a tool's answer. Asking "who calls this function?" via `trace_callers` just returned a list — with no way to tell whether that list matched the code you're actually looking at.

Now, those tools compare the commit the graph was built from against the repo's current state, and prepend a warning when they don't match:

```
⚠ stale: indexed_head=abc1234 current_head=def5678 (branch/commit changed) — run index_project to refresh

<normal tool output follows>
```

No warning appears when the graph is fresh, or when freshness can't be determined at all (e.g. a project that isn't a git repo) — the goal is to flag *known* staleness, not to nag.

## What changed

**New: `infigraph-core::freshness`** ([`crates/infigraph-core/src/freshness.rs`](../crates/infigraph-core/src/freshness.rs))

- `write_index_meta(root)` stamps the current git HEAD into `<root>/.infigraph/index_meta.json` after a successful index. Called from `Infigraph::index()` and `Infigraph::index_files()` ([`crates/infigraph-core/src/lib.rs`](../crates/infigraph-core/src/lib.rs)) — i.e. both a full index and the watcher's incremental batch reindex.
- `compute_freshness(root, pending_changes)` returns a `Freshness { status, indexed_head, current_head, working_tree_dirty, pending_changes }` by comparing the stamped HEAD against `git rev-parse HEAD` and `git status --porcelain` (the latter excludes `.infigraph/` itself — see "Design notes" below).
- `FreshnessStatus` is one of:

  | Status | Meaning |
  |---|---|
  | `Fresh` | Indexed HEAD matches current HEAD, tree is clean, nothing pending. |
  | `Stale` | Indexed HEAD differs from current HEAD, and/or the working tree is dirty. |
  | `Updating` | HEAD matches and the tree is clean, but a watcher has files queued for reindex. |
  | `Unknown` | Not a git repo, or the graph has never been indexed with freshness tracking. Not treated as a warning condition — see "Design notes". |

- `Freshness::warning_line()` renders the `⚠ ...` line shown above, or `None` for `Fresh`/`Unknown`.

**Watcher startup reconciliation** ([`crates/infigraph-core/src/watch/mod.rs`](../crates/infigraph-core/src/watch/mod.rs), `reconcile_on_start`)

Called once, before the watcher's event loop starts. If the stored `indexed_head` differs from the live HEAD, it runs `git diff --name-only <indexed_head> <current_head>` and feeds every changed file into the existing pending-reindex mechanism (the same one `has_cross_file_calls` events use) as if a live file-write event had just been seen for it. This is what makes a restarted watcher catch commits made while it was down, rather than waiting for some future unrelated file write to trigger a reindex.

If `indexed_head` is no longer a reachable git object (e.g. the tree was rewritten with a hard rebase + gc while the watcher was down), `git diff` fails and reconciliation silently no-ops — `compute_freshness` will still correctly report `Stale` on the next query, but the watcher won't proactively queue the affected files until something else touches them. Known, narrow limitation, not yet addressed.

**MCP tool wrapping**

`prepend_freshness_warning(root, output)` ([`crates/infigraph-mcp/src/tools/helpers.rs`](../crates/infigraph-mcp/src/tools/helpers.rs)) computes freshness (folding in the current watcher's pending-reindex count, if one is registered for that path) and prepends the warning line when applicable. Applied to:

- `trace_callers`, `trace_callees`, `transitive_impact` — [`crates/infigraph-mcp/src/tools/analysis/call_graph.rs`](../crates/infigraph-mcp/src/tools/analysis/call_graph.rs)
- `find_all_references` — [`crates/infigraph-mcp/src/tools/graph.rs`](../crates/infigraph-mcp/src/tools/graph.rs)

`get_watch_status` (`crates/infigraph-mcp/src/tools/watch.rs`) also now includes freshness fields when queried with a `watcher_id`.

**Not changed / explicitly out of scope:** the CLI's own `callers`/`callees`/`impact` commands (`crates/infigraph-cli/src/graph_commands.rs`) are a separate implementation from the MCP tools and do not carry this warning. Extending them is a reasonable follow-up but wasn't part of this fix's scope (the driving issue was specifically about MCP tool responses).

## Design notes (for whoever extends this next)

- **`Unknown` is not a warning condition.** A non-git project, or a graph indexed before this feature existed, has no `indexed_head` to compare against — that's "can't tell," not "known stale." If you add a new freshness-consuming call site, don't assume `!= Fresh` means "warn"; check for `Stale`/`Updating` specifically (or use `warning_line()`, which already encodes this).
- **`.infigraph/` is excluded from the dirty-tree check.** It's Infigraph's own runtime state (including `index_meta.json` itself); a project that hasn't gitignored it yet would otherwise always look dirty. Real projects are expected to `.gitignore` it (this repo does), but the check doesn't rely on that.
- **Freshness is per-project-root**, keyed off the same `.infigraph/` directory the graph itself lives in — no additional registry or global state.

## Testing

- Unit tests: [`crates/infigraph-core/tests/freshness.rs`](../crates/infigraph-core/tests/freshness.rs) — every `FreshnessStatus` branch against real temp git repos.
- Integration tests: [`crates/infigraph-mcp/tests/freshness_tools.rs`](../crates/infigraph-mcp/tests/freshness_tools.rs) (all four MCP tools, fresh/stale/dirty) and `test_watcher_restart_reconciles_missed_commit` in [`crates/infigraph-mcp/tests/watcher_reindex.rs`](../crates/infigraph-mcp/tests/watcher_reindex.rs).
- End-to-end simulation: [`docker/freshness-test/`](../docker/freshness-test/) builds real release binaries in a container and drives the MCP HTTP endpoint through all three scenarios (branch switch, process restart, dirty tree). Run manually with:

  ```bash
  docker build -f docker/freshness-test/Dockerfile -t infigraph-freshness-test .
  docker run --rm infigraph-freshness-test
  ```

  Not wired into CI — it's a throwaway sanity harness for this feature, kept for anyone who wants to re-verify the fix after touching related code.
