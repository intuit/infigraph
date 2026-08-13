use anyhow::Result;

use crate::graph::GraphBackend;
use crate::lang::LanguageRegistry;
use crate::Infigraph;

/// Cross-repo resolution for namespace-qualified C++ calls whose target
/// isn't defined in the caller's own repo (e.g. `tps::SetFormML(...)` called
/// from tto-engine-master, defined in TpsBridge-master). Mirrors
/// resolve::calls's within-repo Strategy 1 ({receiver}::{name} lookup) but
/// across the group's independent per-repo graphs, since a symbol defined in
/// one repo is never visible to another repo's own resolve_calls pass.
///
/// Only links when exactly one OTHER repo in the group defines the
/// qualified symbol — ambiguous (0 or 2+) matches are left unresolved,
/// matching this repo's standing policy against guessing on collisions.
///
/// Each repo in a group is its own independent embedded graph (Kuzu local
/// mode) — a `Symbol` node created in the producer repo's database simply
/// does not exist in the caller repo's database, so `CALLS_SERVICE(FROM
/// Symbol TO Symbol)` can never span two different backends directly (a
/// `MATCH` for a node that lives in another graph just yields zero rows,
/// which Kuzu treats as a normal, error-free empty result — not a failure —
/// so checking only `.is_ok()` on the CREATE query is not enough to confirm
/// an edge actually landed). Mirrors `cross_service::link_cross_service_calls`,
/// which solves the identical problem for HTTP cross-service edges by
/// `MERGE`-ing a lightweight proxy `Symbol` node (kind `ExternalService`)
/// into the caller's own graph and pointing `CALLS_SERVICE` at that proxy
/// instead of the producer's real node.
pub fn link_cross_repo_namespace_calls(
    group_backends: &[(&str, &dyn GraphBackend)],
) -> Result<usize> {
    let mut linked = 0usize;

    for (caller_name, caller_backend) in group_backends {
        // Find calls whose receiver is a plausible C++ namespace qualifier
        // and whose target symbol doesn't exist in this repo's own graph.
        // relations.scm already tags these with the qualifier text as the
        // CALLS edge's rel.receiver at extraction time; that's persisted as
        // an EXTERNAL_CALL edge to an ExternalRef node when resolve_calls
        // can't find a local match (see resolve/calls.rs's
        // write_external_calls) — that's exactly the set of candidates to
        // check against sibling repos. Confirmed against the real
        // write_external_calls: ExternalRef nodes are keyed by `id` (the
        // MERGE key, "{receiver}::{method}") and carry `qualifier`/`method`
        // properties set via `ON CREATE SET e.qualifier = ..., e.method = ...`
        // — matching the property names used below.
        let unresolved = caller_backend.raw_query(
            "MATCH (a:Symbol)-[:EXTERNAL_CALL]->(e:ExternalRef) \
             RETURN a.id, e.qualifier, e.method",
        )?;

        for row in &unresolved {
            if row.len() < 3 {
                continue;
            }
            let caller_id = &row[0];
            let qualifier = &row[1];
            let method = &row[2];

            // Only treat lowercase-leading qualifiers as namespace
            // candidates (heuristic: real C++ namespaces in this codebase
            // are lowercase identifiers like `tps`, not receiver
            // variable names, which relations.scm's receiver capture
            // doesn't distinguish from at extraction time).
            if qualifier.is_empty() || !qualifier.chars().next().unwrap().is_lowercase() {
                continue;
            }

            let qualified_suffix = format!("::{qualifier}::{method}");
            let mut matches: Vec<(&str, String)> = Vec::new();

            for (repo_name, repo_backend) in group_backends {
                if repo_name == caller_name {
                    continue;
                }
                let query = format!(
                    "MATCH (s:Symbol) WHERE s.id ENDS WITH '{}' RETURN s.id",
                    qualified_suffix.replace('\'', "\\'")
                );
                let hits = repo_backend.raw_query(&query).unwrap_or_default();
                for hit in hits {
                    if let Some(id) = hit.first() {
                        matches.push((repo_name, id.clone()));
                    }
                }
            }

            if matches.len() != 1 {
                continue; // 0 or ambiguous — leave unresolved, don't guess
            }
            let (target_repo, target_id) = &matches[0];
            // Prefix the proxy id with the target repo name so it can never
            // collide with a REAL local Symbol node in the caller's own graph
            // that happens to share the same relative-path-derived id (e.g.
            // two repos both having `src/common/util.cpp`). Mirrors
            // link_cross_service_calls's `xsvc::{}::{}::{}` proxy-id prefix
            // at cross_service.rs:689 — same idea, different namespace tag
            // since this is a static-lib link, not an HTTP one.
            let proxy_id = format!("xlib::{}::{}", target_repo, target_id);
            let target_id_esc = proxy_id.replace('\'', "\\'");
            let caller_id_esc = caller_id.replace('\'', "\\'");
            let qualifier_esc = qualifier.replace('\'', "\\'");

            // Skip if already linked (idempotent — safe to re-run on an
            // already-processed group, same as link_cross_service_calls).
            let check_edge = format!(
                "MATCH (a:Symbol {{id: '{caller_id_esc}'}})-[:CALLS_SERVICE]->(b:Symbol {{id: '{target_id_esc}'}}) RETURN a.id"
            );
            let existing = caller_backend.raw_query(&check_edge).unwrap_or_default();
            if !existing.is_empty() {
                continue;
            }

            // The real target Symbol node lives in `target_repo`'s own
            // (separate) graph, not the caller's — MERGE a lightweight proxy
            // node into the caller's graph first so CALLS_SERVICE's FROM/TO
            // Symbol constraint can be satisfied within a single backend.
            let docstring = format!(
                "Cross-repo static-lib symbol: {} (defined in '{}')",
                target_id, target_repo
            );
            let create_target = format!(
                "MERGE (t:Symbol {{id: '{target_id_esc}'}}) \
                 ON CREATE SET t.name = '{qualified_suffix_name}', t.kind = 'ExternalService', \
                 t.file = '(external)', t.start_line = 0, t.end_line = 0, \
                 t.signature_hash = '', t.language = 'external', t.visibility = 'public', \
                 t.parent = '', t.docstring = '{docstring_esc}', t.complexity = 0",
                qualified_suffix_name = method.replace('\'', "\\'"),
                docstring_esc = docstring.replace('\'', "\\'"),
            );
            if caller_backend.raw_query(&create_target).is_err() {
                continue;
            }

            let insert = format!(
                "MATCH (a:Symbol {{id: '{caller_id_esc}'}}), (b:Symbol {{id: '{target_id_esc}'}}) \
                 CREATE (a)-[:CALLS_SERVICE {{protocol: 'static_lib', qualifier: '{qualifier_esc}'}}]->(b)"
            );
            if caller_backend.raw_query(&insert).is_ok() {
                // Kuzu returns an empty-but-Ok result when the CREATE's MATCH
                // finds nothing (e.g. a stale caller_id) — `.is_ok()` alone
                // can't tell "edge created" from "silently matched zero
                // rows", so re-run check_edge to confirm the edge actually
                // landed before counting it. (link_cross_service_calls's own
                // `total` counter has this same theoretical gap and only
                // checks `.is_ok()` — this module's doc comment specifically
                // calls the trap out, so it gets the real check here.)
                let confirmed = caller_backend.raw_query(&check_edge).unwrap_or_default();
                if !confirmed.is_empty() {
                    linked += 1;
                }
            }
        }
    }

    Ok(linked)
}

/// Call-site-compatible wrapper around [`link_cross_repo_namespace_calls`] for
/// production use (`group build` / `group link`). The low-level function needs
/// ALL of a group's repo backends open simultaneously (to query siblings for
/// namespace matches), so — unlike `link_cross_service_calls`, which opens one
/// repo's backend at a time internally per caller — this wrapper opens every
/// repo in the group upfront and hands the low-level function borrowed
/// references to all of them at once.
pub fn link_cross_repo_namespace_calls_for_group(
    registry: &crate::multi::Registry,
    group_name: &str,
    build_registry: impl Fn() -> Result<LanguageRegistry>,
) -> Result<usize> {
    let group = registry
        .groups
        .get(group_name)
        .ok_or_else(|| anyhow::anyhow!("group '{group_name}' not found"))?;

    let mut opened: Vec<(String, Infigraph)> = Vec::new();
    for repo_name in &group.repos {
        let entry = match registry.repos.get(repo_name) {
            Some(e) => e,
            None => continue,
        };
        let mut prism = Infigraph::open(&entry.path, build_registry()?)?;
        prism.init()?;
        opened.push((repo_name.clone(), prism));
    }

    let refs: Vec<(&str, &dyn GraphBackend)> = opened
        .iter()
        .filter_map(|(name, prism)| prism.backend().map(|b| (name.as_str(), b)))
        .collect();

    link_cross_repo_namespace_calls(&refs)
}
