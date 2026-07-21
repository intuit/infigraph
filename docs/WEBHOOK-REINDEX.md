# Webhook Auto-Reindex

Automatically reindex the code graph when code changes land in any indexed repository. When a PR is merged (push event), the infigraph MCP HTTP server pulls the latest code, re-indexes the changed repo, and rebuilds the group graph — all in the background while continuing to serve queries from the existing graph.

## How It Works

```
GitHub Enterprise (GHE)
  │
  │  Push event (PR merge)
  │  X-Hub-Signature-256 signed
  │
  ▼
┌─────────────────────────────────────────────────┐
│  infigraph-mcp-service pod                      │
│                                                 │
│  POST /webhook/reindex                          │
│    ├─ Validate HMAC-SHA256 signature            │
│    ├─ Parse push event (repo, branch, ref)      │
│    ├─ Ignore non-default branch pushes          │
│    ├─ Check concurrency guard (AtomicBool)      │
│    └─ Return 200 immediately                    │
│                                                 │
│  Background thread:                             │
│    ├─ git pull --ff-only <changed repo>         │
│    ├─ infigraph index <repo>                    │
│    ├─ infigraph group build <group>             │
│    └─ Log completion + update status            │
│                                                 │
│  During reindex: serve from existing graph      │
│  (stale reads OK, no downtime)                  │
└─────────────────────────────────────────────────┘
```

## Endpoints

### POST /webhook/reindex

Receives GitHub push events and triggers background reindexing.

**Request:** GitHub webhook push event payload with `X-Hub-Signature-256` header.

**Responses:**

| Status | Body | Meaning |
|--------|------|---------|
| 200 | `{"status": "accepted", "repo": "..."}` | Reindex started in background |
| 200 | `{"status": "ignored", "reason": "non-default branch"}` | Push to non-default branch, no action |
| 200 | `{"status": "queued", "reason": "reindex already in progress"}` | Another reindex is running |
| 400 | `{"error": "Invalid JSON"}` | Unparseable request body |
| 401 | `{"error": "Invalid signature"}` | HMAC validation failed |

### GET /webhook/status

Returns current reindex state.

**Response:**

```json
{
  "reindexing": false,
  "last_repo": "skills-registry",
  "last_result": "success",
  "last_completed_epoch": 1752883200
}
```

| Field | Type | Description |
|-------|------|-------------|
| `reindexing` | bool | Whether a reindex is currently running |
| `last_repo` | string | Name of the last repo that triggered a reindex |
| `last_result` | string | `"success"`, `"partial_failure"`, or `"in_progress"` |
| `last_completed_epoch` | u64 | Unix timestamp of last completion (0 if never completed) |

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `WEBHOOK_SECRET` | *(none)* | HMAC secret for X-Hub-Signature-256 validation. **If unset, signature validation is skipped** (fail-open for local dev). |
| `GROUP_NAME` | `org` | Infigraph group name for group build |
| `CLONE_DIR` | `/app/data/repos` | Directory where repos are cloned |
| `INFIGRAPH_BIN` | `/app/infigraph` | Path to the infigraph CLI binary |

### Kubernetes Secrets

| Secret | Mount Path | Description |
|--------|-----------|-------------|
| `webhook-secret` | `/etc/secrets/webhook-secret` | GHE webhook HMAC secret |
| `ghe-token` | `/etc/secrets/ghe-token` | Service account PAT for git operations |

### entry.sh Integration

Add to `entry.sh` before starting the MCP server:

```bash
# Read webhook secret from K8s secrets mount
if [ -f /etc/secrets/webhook-secret ]; then
  export WEBHOOK_SECRET=$(cat /etc/secrets/webhook-secret)
fi
```

## GitHub Enterprise Webhook Setup

### Organization Webhook

| Field | Value |
|-------|-------|
| Payload URL | `https://<your-service-url>/webhook/reindex` |
| Content type | `application/json` |
| Secret | Same value as `WEBHOOK_SECRET` env var |
| Events | **Pushes** only |
| Active | Yes |

### Per-Repository Webhook (Alternative)

For selective repos, configure webhooks on individual repositories instead of the org level.

## Security

### HMAC-SHA256 Signature Validation

Every webhook request is validated using the `X-Hub-Signature-256` header:

1. GitHub computes `HMAC-SHA256(secret, request_body)` and sends it as `sha256=<hex>`
2. Server recomputes the HMAC and compares using constant-time comparison
3. Mismatched signatures return 401

### Fail-Open Behavior

When `WEBHOOK_SECRET` is **not set**, all webhook requests are accepted without signature validation. This is intentional for local development but **must not be used in production**.

**Production checklist:**
- [ ] `WEBHOOK_SECRET` is set and non-empty
- [ ] Secret is stored as a Kubernetes secret, not in plaintext
- [ ] Secret matches the GitHub webhook configuration
- [ ] HTTPS is enforced (webhook payloads contain repo metadata)

### No Auth Header Required

The webhook endpoint does not require the `Authorization: Bearer <key>` header used by other MCP endpoints. Authentication is handled entirely through the HMAC signature, which is the standard mechanism for GitHub webhooks.

## Error Handling

The background reindex thread handles failures gracefully:

| Step | On Failure | Effect |
|------|-----------|--------|
| `git pull --ff-only` | Logs ERROR, continues | Index runs on stale code |
| `infigraph index` | Logs ERROR, continues | Group build runs with stale repo graph |
| `infigraph group build` | Logs ERROR, continues | Cross-repo links may be stale |

In all cases:
- The `REINDEXING` flag is cleared (prevents deadlock)
- The status endpoint reports `"partial_failure"`
- The server continues serving queries from the existing graph

## Concurrency

A static `AtomicBool` (`REINDEXING`) prevents concurrent reindex operations:

- First webhook → `compare_exchange(false, true)` succeeds → reindex starts
- Second webhook during reindex → `compare_exchange` fails → returns `"queued"`
- On completion (success or failure) → flag reset to `false`

This means rapid pushes are coalesced: only one reindex runs at a time, and subsequent pushes are acknowledged but not processed until the current reindex completes.

## Decision Logic

The handler separates pure decision logic from side effects for testability:

```rust
enum WebhookDecision {
    Reject401,           // Bad/missing HMAC signature
    BadJson400,          // Unparseable request body
    Ignored { reason },  // Non-default branch push
    AlreadyReindexing,   // Concurrent reindex guard
    Accepted { repo, clone_dir, group, bin },
}

fn decide_webhook(body, signature, reindexing) -> WebhookDecision
```

The HTTP handler is a thin wrapper: read body → extract signature header → call `decide_webhook` → match result → spawn thread only on `Accepted`.

## Testing

17 tests cover the webhook functionality:

### HMAC Validation (6 tests)
- Valid signature accepted
- Wrong signature rejected
- Missing signature rejected (when secret configured)
- No secret configured → pass (fail-open)
- Missing `sha256=` prefix rejected
- Non-hex signature rejected

### Decision Logic (7 tests)
- Bad JSON → 400
- Non-default branch → ignored
- Default branch push → accepted
- Already reindexing → queued
- Missing repo name → accepted with empty string
- Custom default branch (e.g., `develop`)
- Bad signature → 401

### HTTP Integration (4 tests)
- POST valid push → 200 accepted
- POST bad signature → 401
- GET /webhook/status → 200
- REINDEXING flag cleared after spawn (even on failure)

## Production Roadmap

### Phase 1: Debounced Reindex Queue
Multiple repos push within minutes → collapse into one rebuild with a configurable debounce window (default 60s).

### Phase 2: Tiered Reindex
After per-repo index, check if API contracts changed. Skip expensive `group build` when contracts are unchanged (~80% of pushes).

### Phase 3: Incremental Combined Graph
Only merge diffs from changed repos into the existing combined graph instead of full rebuild.

### Phase 4: GitHub App
Replace org webhook + service account PAT with a GitHub App for auto-discovery, scoped permissions, and short-lived installation tokens.

### Phase 5: Multi-Pod Scaling
Leader election for webhook routing, sharded repo ownership, combined graph built by leader.

See [PLAN-webhook-reindex.md](PLAN-webhook-reindex.md) for detailed design of each phase.
