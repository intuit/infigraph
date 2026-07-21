# Webhook Reindex Plan

## Overview

When code changes land in any indexed repo (PR merge → push event), the infigraph-mcp-service pod must update its graph to reflect the new state. This plan covers the full spectrum from demo MVP to production-grade incremental reindex.

## Architecture

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
│    ├─ Validate signature (HMAC-SHA256)          │
│    ├─ Parse push event (repo name, branch, ref) │
│    ├─ Enqueue reindex job                       │
│    └─ Return 200 immediately (async processing) │
│                                                 │
│  Reindex Worker (background thread)             │
│    ├─ git pull <changed repo>                   │
│    ├─ infigraph index <repo>                    │
│    ├─ infigraph group sync <group>              │
│    ├─ infigraph group link <group>              │
│    ├─ infigraph group combined <group>          │
│    └─ Log completion                            │
│                                                 │
│  During reindex: serve from existing graph      │
│  (stale reads OK, no downtime)                  │
└─────────────────────────────────────────────────┘
```

## Reindex Tiers

### Tier 1: Per-Repo Reindex (fast, partial)

**Trigger:** Push event for a single repo
**Steps:**
1. `git pull` the changed repo (fast — shallow clone already exists)
2. `infigraph index <repo-path>` — rebuild that repo's code graph only

**Time:** ~10-30s per repo
**Result:** Single repo graph is fresh. Combined graph is stale.
**Use case:** Demo MVP. Quick feedback. Queries against individual repo are fresh.

### Tier 2: Group Sync + Link (medium, contracts updated)

**Trigger:** After Tier 1 completes
**Steps:**
3. `infigraph group sync <group>` — re-extract API contracts from all repos
4. `infigraph group link <group>` — re-link cross-service CALLS_SERVICE edges

**Time:** ~1-2 min for 30 repos
**Result:** Cross-service contracts and call edges are fresh. Combined search index still stale.
**Use case:** When cross-service contract accuracy matters (API changes, new endpoints).

### Tier 3: Combined Graph Rebuild (slow, fully consistent)

**Trigger:** After Tier 2, or on schedule, or manual
**Steps:**
5. `infigraph group combined <group>` — rebuild unified search index across all repos

**Time:** ~3-5 min for 30 repos
**Result:** Everything fresh. group_search returns results from latest code.
**Use case:** Full consistency. Required after major refactors or new repos added.

### Shortcut: `group build` (all tiers in one)

`infigraph group build <group>` runs: index all → sync → link → combined.
Equivalent to Tier 1 (all repos) + Tier 2 + Tier 3.
**Time:** ~5-10 min for 30 repos.

## Demo MVP (Wed 7/16)

### What to implement

**Endpoint:** `POST /webhook/reindex` on the MCP HTTP server

**Behavior:**
1. Validate `X-Hub-Signature-256` header using `WEBHOOK_SECRET` env var
2. Parse push event JSON: extract `repository.name`, `ref`, `repository.default_branch`
3. Ignore pushes to non-default branches
4. Return 200 immediately
5. Background: `git pull` → `infigraph index` → `infigraph group build` (full rebuild)
6. During rebuild: continue serving from existing graph (no 503, no downtime)
7. Log progress with structured logging

**Why full rebuild for demo:**
- Simpler — one code path
- 30 repos × ~10s index = ~5 min total, acceptable for demo
- Avoids partial-state edge cases
- `group build` is idempotent and battle-tested

**Webhook setup:**
- GHE org webhook on `context-rag` org → push events only
- Secret stored as K8s secret, mounted at `/etc/secrets/webhook-secret`
- URL: `https://infigraph-mcp-service-e2e.api.intuit.com/webhook/reindex`

### Implementation locations

| Component | File | Change |
|-----------|------|--------|
| Webhook endpoint | `crates/infigraph-mcp/src/web/mod.rs` | Add POST /webhook/reindex route |
| HMAC validation | `crates/infigraph-mcp/src/web/mod.rs` | New `validate_webhook_signature()` fn |
| Background reindex | `crates/infigraph-mcp/src/web/mod.rs` | Spawn thread, shell out to `infigraph` CLI |
| Reindex lock | `crates/infigraph-mcp/src/web/mod.rs` | AtomicBool to prevent concurrent reindexes |
| entry.sh | `package/entry.sh` | Read WEBHOOK_SECRET from secrets mount |
| Dockerfile | `Dockerfile` | No changes needed (infigraph CLI already included) |

### Rust implementation sketch

```rust
// In web/mod.rs

use std::sync::atomic::AtomicBool;
use std::process::Command;

static REINDEXING: AtomicBool = AtomicBool::new(false);

// Route: POST /webhook/reindex (no auth — uses webhook signature instead)
fn handle_webhook_reindex(request: &mut tiny_http::Request) -> Response<...> {
    // 1. Read body
    let mut body = String::new();
    request.as_reader().read_to_string(&mut body);

    // 2. Validate HMAC-SHA256 signature
    let signature = request.headers().iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("x-hub-signature-256"))
        .map(|h| h.value.as_str().to_string());
    if !validate_webhook_signature(&body, signature.as_deref()) {
        return serve_json_status(401, json!({"error": "Invalid signature"}));
    }

    // 3. Parse push event
    let event: Value = serde_json::from_str(&body)?;
    let repo_name = event.pointer("/repository/name").and_then(|v| v.as_str());
    let ref_str = event.get("ref").and_then(|v| v.as_str());
    let default_branch = event.pointer("/repository/default_branch").and_then(|v| v.as_str());

    // 4. Ignore non-default-branch pushes
    let branch_ref = format!("refs/heads/{}", default_branch.unwrap_or("main"));
    if ref_str != Some(&branch_ref) {
        return serve_json_status(200, json!({"status": "ignored", "reason": "non-default branch"}));
    }

    // 5. Prevent concurrent reindexes
    if REINDEXING.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return serve_json_status(200, json!({"status": "queued", "reason": "reindex already in progress"}));
    }

    // 6. Spawn background reindex
    let repo = repo_name.unwrap_or("unknown").to_string();
    thread::spawn(move || {
        // git pull
        let repo_path = format!("/app/data/repos/{}", repo);
        Command::new("git").args(&["-C", &repo_path, "pull", "--ff-only"]).status().ok();

        // reindex single repo
        Command::new("/app/infigraph").args(&["index"]).current_dir(&repo_path).status().ok();

        // rebuild group (sync + link + combined)
        let group = std::env::var("GROUP_NAME").unwrap_or_else(|_| "org".to_string());
        Command::new("/app/infigraph").args(&["group", "build", &group]).status().ok();

        REINDEXING.store(false, Ordering::SeqCst);
    });

    serve_json_status(200, json!({"status": "accepted", "repo": repo_name}))
}

fn validate_webhook_signature(body: &str, signature: Option<&str>) -> bool {
    let secret = match std::env::var("WEBHOOK_SECRET") {
        Ok(s) => s,
        Err(_) => return true, // No secret configured = skip validation
    };
    let sig = match signature {
        Some(s) => s,
        None => return false,
    };
    // sig format: "sha256=<hex>"
    let expected = sig.strip_prefix("sha256=").unwrap_or("");
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let computed = hex::encode(mac.finalize().into_bytes());
    computed == expected
}
```

### New dependencies needed

```toml
# crates/infigraph-mcp/Cargo.toml
hmac = "0.12"
sha2 = "0.10"
hex = "0.4"
```

## Production Roadmap (Post-Demo)

### Phase 1: Debounced Reindex Queue

**Problem:** Multiple repos push within minutes → N full rebuilds queued.
**Solution:** Debounce queue with configurable window (default 60s):
- Push event arrives → add repo to dirty set, reset debounce timer
- Timer fires → pull all dirty repos, single `group build`
- Concurrent pushes collapse into one rebuild

```
Push repo-A (t=0)  → dirty={A}, timer starts (60s)
Push repo-B (t=5)  → dirty={A,B}, timer resets (60s from now)
Push repo-C (t=10) → dirty={A,B,C}, timer resets
Timer fires (t=70) → pull A,B,C → group build → dirty={}
```

### Phase 2: Tiered Reindex

**Trigger:** Single push event
**Steps:**
1. **Immediate (Tier 1):** `git pull` + `infigraph index` on changed repo (~30s)
2. **Deferred (Tier 2+3):** After Tier 1, check if contracts changed (diff .infigraph/contracts/). If yes → `group sync` + `group link` + `group combined`. If no → skip.

**Benefit:** Most pushes don't change API contracts. Skip expensive combined rebuild 80%+ of the time.

### Phase 3: Incremental Combined Graph

**Current limitation:** `group combined` does a full rebuild — re-reads all repo graphs, merges into one.
**Target:** Only merge the diff from changed repos into existing combined graph.
**Requires:** Core changes to infigraph — combined graph needs INSERT/UPDATE/DELETE, not full rebuild.
**Estimate:** Medium effort. Needs ALTER TABLE or graph diffing in kuzu.

### Phase 4: GitHub App (replaces org webhook + service account)

**Current:** Org-level webhook + service account PAT
**Target:** GitHub App installed on org:
- Auto-discovers repos (installation events)
- Receives push events per-repo (no org webhook needed)
- Mints short-lived installation tokens (no long-lived PAT)
- Can react to repo created/deleted/archived events
- Scoped permissions (Contents:Read, Metadata:Read)

**Setup:**
1. Create GitHub App on GHE (`infigraph-indexer`)
2. Request installation on `context-rag` org
3. entry.sh mints installation token at startup via App private key
4. Webhook URL configured in App settings (not org settings)

### Phase 5: Multi-Pod Scaling

**Current:** Single pod indexes everything.
**Target:** Multiple pods with sharded responsibility:
- Leader election via K8s lease
- Leader handles webhook → decides which pod reindexes which repos
- Each pod owns a shard of repos
- Combined graph built by leader after all shards updated

**Alternative:** Single indexer pod + multiple read-only serving pods. Indexer builds graph on shared PV, serving pods mount read-only.

## Configuration Reference

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `WEBHOOK_SECRET` | (none) | HMAC secret for X-Hub-Signature-256 validation |
| `GROUP_NAME` | `org` | Infigraph group name for combined graph |
| `CLONE_DIR` | `/app/data/repos` | Where repos are cloned |
| `REINDEX_DEBOUNCE_SECS` | `60` | (Phase 1) Debounce window for batching pushes |

### Secrets

| Secret | Mount Path | Description |
|--------|-----------|-------------|
| `webhook-secret` | `/etc/secrets/webhook-secret` | GHE webhook HMAC secret |
| `ghe-token` | `/etc/secrets/ghe-token` | Service account PAT for git operations |
| `api-key` | `/etc/secrets/api-key` | Bearer token for MCP API auth |

### GHE Org Webhook Config

| Field | Value |
|-------|-------|
| Payload URL | `https://infigraph-mcp-service-e2e.api.intuit.com/webhook/reindex` |
| Content type | `application/json` |
| Secret | Same as `WEBHOOK_SECRET` env var |
| Events | Push only |
| Active | Yes |

## Execution Order

### Demo (Wed 7/16)

1. ✅ entry.sh orchestrator (clone → group build → signal ready)
2. ✅ Readiness-gated /health (503 → 200)
3. ✅ repos.yaml ConfigMap
4. ✅ Dockerfile (both binaries, models, PV)
5. ✅ POST /webhook/reindex (HMAC validation, background reindex, error handling, status endpoint)
6. ✅ Add hmac/sha2/hex deps to Cargo.toml
7. ⬜ entry.sh: read WEBHOOK_SECRET from secrets mount
8. ⬜ GHE org webhook pointing to service
9. ⬜ Request service account + webhook secret

### Post-Demo

10. ⬜ Debounced reindex queue (Phase 1)
11. ⬜ Tiered reindex with contract-change detection (Phase 2)
12. ⬜ Incremental combined graph (Phase 3)
13. ⬜ GitHub App (Phase 4)
14. ⬜ Multi-pod scaling (Phase 5)
