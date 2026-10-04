# Read-only diagnostics

`infigraph doctor` prints a concise report. `infigraph doctor --json` returns
schema version 1 JSON, with stable codes, status, severity, explanation, and
recommended action. `infigraph --root <project> doctor` selects a project.
The MCP `diagnose` tool accepts a required `path` and returns the same JSON as
text in the existing MCP response format, without compression.

Exit codes: 0 healthy, 1 degraded (warnings), 2 unhealthy (errors). Informational
checks, including unsupported optional capabilities, do not change health.
The human summary prints index freshness beside overall health; a recorded
revision match still prints UNKNOWN for working-tree freshness.
UNKNOWN is explicit; a healthy report means the implemented required checks
passed, not that every optional capability or index freshness was verified.

For example, when `INDEX_STALE` reports that HEAD differs from the registry's
recorded indexed revision, run `infigraph index`, then rerun doctor. Doctor never
executes its recommendations.

## Design and proposed checks (upstream main cf82f5d)

The shared typed engine lives in `infigraph-core::diagnostics`. CLI and MCP only
render its results. Checks run in fixed order and never print raw config, source,
registry contents, lock payloads, backend credentials, or native error messages.

| Area | Existing state/API | Check and limits |
| --- | --- | --- |
| Project | Canonical project root, `.infigraph/graph` | Root readable, graph exists. Non-Git projects are supported. |
| Graph | `GraphStore::open_read_only_with_config`, `GraphBackend::stats` | Read-only schema/statistics query in an isolated bounded probe. No `init`, recovery, schema creation, or writes. Reject non-files/truncated graphs; skip any WAL-family sibling. Native probe failure is reported, never repaired. |
| Locks | `lockfile`, fs2 advisory flock | Shared nonblocking observation of existing files only; never create/stamp/truncate locks. Held graph write lock skips graph inspection. Held watch lock indicates an owner, not freshness or process progress. |
| Registry | `Registry::load`, `RepoEntry::last_indexed_commit`, `git_head_commit` | Match canonical project paths; compare recorded revision to HEAD. The registry is not atomic graph provenance and watchers do not update this field. A match does not establish working-tree freshness. Missing metadata is UNKNOWN. |
| Watcher | Existing project `watch.lock`; MCP `WATCHERS` | Watcher absent is optional INFO; MCP adds pending reindex count from its own registry. No PID manager, duplicate detector, or stale PID inference: current watch locks have no identity payload. |
| Search | `.infigraph/embeddings.bin`, `embedding_count_checked` | File/header readability and stored count only; contents/model/HNSW validity remain unknown. Missing persisted assets are optional INFO (on-demand embedding fallback). Unreadable persisted headers are WARNING because current search propagates asset loader errors. No model loading, mmap, downloads, or index building. |
| MCP | Installer's sibling/PATH binary discovery | Reuse discovery from a small shared module. Missing binary is INFO for CLI-only installations. |
| Integrations | CLI-only target registry being replaced in #63 | Informational UNKNOWN; do not copy agent config formats into core or MCP. |
| Remote | `INFIGRAPH_BACKEND` | Remote graph/registry reported UNKNOWN, with no connection, schema initialization, or network calls. |

## Safety and overlap

PR #53 owns richer freshness, #64 owns general WAL/dead-owner safety, #43 owns
watcher lifecycle, and #63 owns installation artifacts. Doctor does not replace
any of these. Its conservative WAL skip applies even to a live writer. It uses
a dedicated child probe because the native database parser can terminate a
process before returning a Rust error. Both binaries handle the probe before
normal startup (including the MCP supervisor's crash/reindex behavior).

Diagnostics are observations, not an atomic snapshot. A lock probe temporarily
holds a shared advisory lock without changing its contents; files may change
between checks. Unexpected native failures/timeouts are UNKNOWN. No automatic
full rebuild, process termination of watchers, lock removal, config edits, or
repair. Only doctor's own timed-out child probe may be terminated. The probe uses a
64 MiB buffer pool, 1 GiB database mapping budget, two query threads, and a
10-second timeout. Unix also caps address space at 4 GiB (or the stricter
inherited limit) and disables core dumps. Graphs that exceed the probe's
resource budget are UNKNOWN, not declared corrupt.

## Limitations

No proof of working-tree freshness, extractor/schema version compatibility,
watcher PID/progress, live remote connectivity, complete embedding validity,
document graph validity, agent registration correctness, or managed artifact
hashes. Future checks should consume the owning APIs when those exist.

The isolated native probe is available from the `infigraph` and `infigraph-mcp`
executables. Other programs embedding core, or renamed executables, report
`GRAPH_STATUS_UNKNOWN` rather than launching an unsupported host's startup.
Windows applies the database/thread budgets and timeout; the address-space and
core-dump limits are Unix-specific.

## Follow-up

Tracked in [#79](https://github.com/intuit/infigraph/issues/79): after #63 lands,
consume its integration/artifact registry through an explicitly
read-only inspection API for client registration and recorded managed hashes.
After #53 lands, use its freshness observations instead of adding a competing
freshness tracker. Those extensions must preserve the same no-repair contract.

Workspace validation also exposed an independent document-store native abort on
clean upstream main, tracked in [#80](https://github.com/intuit/infigraph/issues/80).
