use kuzu::Connection;

/// Escape single quotes and control characters for Kuzu string literals.
pub(crate) fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', " ")
        .replace('\r', "")
        .replace('\t', " ")
}

/// Convert a path to forward-slash form (needed on Windows for Kuzu COPY FROM).
pub(crate) fn fwd_slash_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Batch-insert edges via UNWIND in chunks of 500.
pub(crate) fn unwind_edges_from_pairs(
    conn: &Connection,
    pairs: &[(&str, &str)],
    rel_type: &str,
    src_label: &str,
    dst_label: &str,
) {
    const CHUNK: usize = 500;
    for chunk in pairs.chunks(CHUNK) {
        let pair_list: Vec<String> = chunk
            .iter()
            .map(|(a, b)| format!("{{a: '{}', b: '{}'}}", escape(a), escape(b)))
            .collect();
        let _ = conn.query(&format!(
            "UNWIND [{}] AS p MATCH (a:{src_label}), (b:{dst_label}) WHERE a.id = p.a AND b.id = p.b CREATE (a)-[:{rel_type}]->(b)",
            pair_list.join(", ")
        ));
    }
}
