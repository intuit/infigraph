//! Embedder identity sidecar: switching embedding provider/model must
//! invalidate previously built vectors even when the dimension is unchanged.

use infigraph_core::embed::{embedder_identity_stale, write_embedder_identity};

#[test]
fn missing_record_is_stale() {
    // Fail-safe: embeddings of unknown provenance must be rebuilt. Callers
    // only check staleness when embeddings already exist, so a fresh index
    // never hits this path.
    let dir = tempfile::tempdir().unwrap();
    assert!(embedder_identity_stale(dir.path(), "local:256"));
    assert!(embedder_identity_stale(
        dir.path(),
        "voyage:voyage-code-3:256"
    ));
}

#[test]
fn same_identity_is_not_stale() {
    let dir = tempfile::tempdir().unwrap();
    write_embedder_identity(dir.path(), "voyage:voyage-code-3:256").unwrap();
    assert!(!embedder_identity_stale(
        dir.path(),
        "voyage:voyage-code-3:256"
    ));
}

#[test]
fn provider_switch_is_stale_at_same_dimension() {
    let dir = tempfile::tempdir().unwrap();
    write_embedder_identity(dir.path(), "local:256").unwrap();
    assert!(embedder_identity_stale(
        dir.path(),
        "voyage:voyage-code-3:256"
    ));
}

#[test]
fn model_switch_is_stale_within_same_provider() {
    let dir = tempfile::tempdir().unwrap();
    write_embedder_identity(dir.path(), "voyage:voyage-code-3:256").unwrap();
    assert!(embedder_identity_stale(
        dir.path(),
        "voyage:voyage-3-large:256"
    ));
}
