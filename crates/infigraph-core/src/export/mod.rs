//! Export the code graph to various formats: Neo4j Cypher, GraphML, JSON.

use std::io::Write;

use anyhow::Result;

use crate::graph::GraphBackend;

/// Escape a string for use in a Cypher string literal (single-quoted).
fn cypher_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Escape a string for use in an XML attribute value.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Escape a string for JSON output (handles quotes, backslashes, control chars).
fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Write Neo4j-compatible Cypher CREATE statements for all nodes and edges.
///
/// Produces output like:
/// ```text
/// CREATE (s:Symbol {id: '...', name: '...', kind: '...', ...});
/// CREATE (s1)-[:CALLS]->(s2);
/// ```
pub fn export_cypher<W: Write>(backend: &dyn GraphBackend, writer: &mut W) -> Result<()> {
    writeln!(writer, "// Infigraph graph export — Neo4j Cypher")?;
    writeln!(writer)?;

    // ── Module nodes ──
    let modules = backend.raw_query("MATCH (m:Module) RETURN m.id, m.name, m.file, m.language")?;
    writeln!(writer, "// Modules ({} nodes)", modules.len())?;
    for row in &modules {
        let id = cypher_escape(&row[0]);
        let name = cypher_escape(&row[1]);
        let file = cypher_escape(&row[2]);
        let language = cypher_escape(&row[3]);
        writeln!(
            writer,
            "CREATE (:Module {{id: '{id}', name: '{name}', file: '{file}', language: '{language}'}});"
        )?;
    }
    writeln!(writer)?;

    // ── Symbol nodes ──
    let symbols = backend.raw_query(
        "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.start_line, s.end_line, s.language, s.visibility, s.parent, s.docstring",
    )?;
    writeln!(writer, "// Symbols ({} nodes)", symbols.len())?;
    for row in &symbols {
        let id = cypher_escape(&row[0]);
        let name = cypher_escape(&row[1]);
        let kind = cypher_escape(&row[2]);
        let file = cypher_escape(&row[3]);
        let start_line = &row[4];
        let end_line = &row[5];
        let language = cypher_escape(&row[6]);
        let visibility = cypher_escape(&row[7]);
        let parent = cypher_escape(&row[8]);
        let docstring = cypher_escape(&row[9]);
        writeln!(
            writer,
            "CREATE (:Symbol {{id: '{id}', name: '{name}', kind: '{kind}', file: '{file}', start_line: {start_line}, end_line: {end_line}, language: '{language}', visibility: '{visibility}', parent: '{parent}', docstring: '{docstring}'}});"
        )?;
    }
    writeln!(writer)?;

    // ── Edges ──
    // CONTAINS (Module -> Symbol)
    let contains =
        backend.raw_query("MATCH (m:Module)-[:CONTAINS]->(s:Symbol) RETURN m.id, s.id")?;
    writeln!(writer, "// CONTAINS edges ({} edges)", contains.len())?;
    for row in &contains {
        let src = cypher_escape(&row[0]);
        let dst = cypher_escape(&row[1]);
        writeln!(
            writer,
            "MATCH (a:Module {{id: '{src}'}}), (b:Symbol {{id: '{dst}'}}) CREATE (a)-[:CONTAINS]->(b);"
        )?;
    }
    writeln!(writer)?;

    // CALLS (Symbol -> Symbol)
    let calls = backend.raw_query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")?;
    writeln!(writer, "// CALLS edges ({} edges)", calls.len())?;
    for row in &calls {
        let src = cypher_escape(&row[0]);
        let dst = cypher_escape(&row[1]);
        writeln!(
            writer,
            "MATCH (a:Symbol {{id: '{src}'}}), (b:Symbol {{id: '{dst}'}}) CREATE (a)-[:CALLS]->(b);"
        )?;
    }
    writeln!(writer)?;

    // INHERITS (Symbol -> Symbol)
    let inherits =
        backend.raw_query("MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) RETURN a.id, b.id")?;
    writeln!(writer, "// INHERITS edges ({} edges)", inherits.len())?;
    for row in &inherits {
        let src = cypher_escape(&row[0]);
        let dst = cypher_escape(&row[1]);
        writeln!(
            writer,
            "MATCH (a:Symbol {{id: '{src}'}}), (b:Symbol {{id: '{dst}'}}) CREATE (a)-[:INHERITS]->(b);"
        )?;
    }
    writeln!(writer)?;

    // TESTED_BY (Symbol -> Symbol)
    let tested_by =
        backend.raw_query("MATCH (a:Symbol)-[:TESTED_BY]->(b:Symbol) RETURN a.id, b.id")?;
    writeln!(writer, "// TESTED_BY edges ({} edges)", tested_by.len())?;
    for row in &tested_by {
        let src = cypher_escape(&row[0]);
        let dst = cypher_escape(&row[1]);
        writeln!(
            writer,
            "MATCH (a:Symbol {{id: '{src}'}}), (b:Symbol {{id: '{dst}'}}) CREATE (a)-[:TESTED_BY]->(b);"
        )?;
    }

    Ok(())
}

/// Write GraphML XML format (compatible with Gephi/yEd).
///
/// Includes all node properties as `<data>` elements with declared `<key>` definitions.
pub fn export_graphml<W: Write>(backend: &dyn GraphBackend, writer: &mut W) -> Result<()> {
    writeln!(writer, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        writer,
        r#"<graphml xmlns="http://graphml.graphstruct.org/graphml"
         xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"
         xsi:schemaLocation="http://graphml.graphstruct.org/graphml http://graphml.graphstruct.org/xmlns/1.0/graphml.xsd">"#
    )?;

    // Key definitions for node properties
    writeln!(
        writer,
        r#"  <key id="d_name" for="node" attr.name="name" attr.type="string"/>"#
    )?;
    writeln!(
        writer,
        r#"  <key id="d_kind" for="node" attr.name="kind" attr.type="string"/>"#
    )?;
    writeln!(
        writer,
        r#"  <key id="d_file" for="node" attr.name="file" attr.type="string"/>"#
    )?;
    writeln!(
        writer,
        r#"  <key id="d_language" for="node" attr.name="language" attr.type="string"/>"#
    )?;
    writeln!(
        writer,
        r#"  <key id="d_start_line" for="node" attr.name="start_line" attr.type="int"/>"#
    )?;
    writeln!(
        writer,
        r#"  <key id="d_end_line" for="node" attr.name="end_line" attr.type="int"/>"#
    )?;
    writeln!(
        writer,
        r#"  <key id="d_visibility" for="node" attr.name="visibility" attr.type="string"/>"#
    )?;
    writeln!(
        writer,
        r#"  <key id="d_parent" for="node" attr.name="parent" attr.type="string"/>"#
    )?;
    writeln!(
        writer,
        r#"  <key id="d_docstring" for="node" attr.name="docstring" attr.type="string"/>"#
    )?;
    writeln!(
        writer,
        r#"  <key id="d_node_type" for="node" attr.name="node_type" attr.type="string"/>"#
    )?;

    // Key definition for edge label
    writeln!(
        writer,
        r#"  <key id="d_label" for="edge" attr.name="label" attr.type="string"/>"#
    )?;
    writeln!(writer)?;
    writeln!(writer, r#"  <graph id="infigraph" edgedefault="directed">"#)?;

    // ── Module nodes ──
    let modules = backend.raw_query("MATCH (m:Module) RETURN m.id, m.name, m.file, m.language")?;
    for row in &modules {
        let id = xml_escape(&row[0]);
        let name = xml_escape(&row[1]);
        let file = xml_escape(&row[2]);
        let language = xml_escape(&row[3]);
        writeln!(writer, r#"    <node id="{id}">"#)?;
        writeln!(writer, r#"      <data key="d_node_type">Module</data>"#)?;
        writeln!(writer, r#"      <data key="d_name">{name}</data>"#)?;
        writeln!(writer, r#"      <data key="d_file">{file}</data>"#)?;
        writeln!(writer, r#"      <data key="d_language">{language}</data>"#)?;
        writeln!(writer, r#"    </node>"#)?;
    }

    // ── Symbol nodes ──
    let symbols = backend.raw_query(
        "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.start_line, s.end_line, s.language, s.visibility, s.parent, s.docstring",
    )?;
    for row in &symbols {
        let id = xml_escape(&row[0]);
        let name = xml_escape(&row[1]);
        let kind = xml_escape(&row[2]);
        let file = xml_escape(&row[3]);
        let start_line = &row[4];
        let end_line = &row[5];
        let language = xml_escape(&row[6]);
        let visibility = xml_escape(&row[7]);
        let parent = xml_escape(&row[8]);
        let docstring = xml_escape(&row[9]);
        writeln!(writer, r#"    <node id="{id}">"#)?;
        writeln!(writer, r#"      <data key="d_node_type">Symbol</data>"#)?;
        writeln!(writer, r#"      <data key="d_name">{name}</data>"#)?;
        writeln!(writer, r#"      <data key="d_kind">{kind}</data>"#)?;
        writeln!(writer, r#"      <data key="d_file">{file}</data>"#)?;
        writeln!(
            writer,
            r#"      <data key="d_start_line">{start_line}</data>"#
        )?;
        writeln!(writer, r#"      <data key="d_end_line">{end_line}</data>"#)?;
        writeln!(writer, r#"      <data key="d_language">{language}</data>"#)?;
        if !visibility.is_empty() {
            writeln!(
                writer,
                r#"      <data key="d_visibility">{visibility}</data>"#
            )?;
        }
        if !parent.is_empty() {
            writeln!(writer, r#"      <data key="d_parent">{parent}</data>"#)?;
        }
        if !docstring.is_empty() {
            writeln!(
                writer,
                r#"      <data key="d_docstring">{docstring}</data>"#
            )?;
        }
        writeln!(writer, r#"    </node>"#)?;
    }

    // ── Edges ──
    let mut edge_id: u64 = 0;

    let contains =
        backend.raw_query("MATCH (m:Module)-[:CONTAINS]->(s:Symbol) RETURN m.id, s.id")?;
    for row in &contains {
        let src = xml_escape(&row[0]);
        let dst = xml_escape(&row[1]);
        writeln!(
            writer,
            r#"    <edge id="e{edge_id}" source="{src}" target="{dst}"><data key="d_label">CONTAINS</data></edge>"#
        )?;
        edge_id += 1;
    }

    let calls = backend.raw_query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")?;
    for row in &calls {
        let src = xml_escape(&row[0]);
        let dst = xml_escape(&row[1]);
        writeln!(
            writer,
            r#"    <edge id="e{edge_id}" source="{src}" target="{dst}"><data key="d_label">CALLS</data></edge>"#
        )?;
        edge_id += 1;
    }

    let inherits =
        backend.raw_query("MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) RETURN a.id, b.id")?;
    for row in &inherits {
        let src = xml_escape(&row[0]);
        let dst = xml_escape(&row[1]);
        writeln!(
            writer,
            r#"    <edge id="e{edge_id}" source="{src}" target="{dst}"><data key="d_label">INHERITS</data></edge>"#
        )?;
        edge_id += 1;
    }

    let tested_by =
        backend.raw_query("MATCH (a:Symbol)-[:TESTED_BY]->(b:Symbol) RETURN a.id, b.id")?;
    for row in &tested_by {
        let src = xml_escape(&row[0]);
        let dst = xml_escape(&row[1]);
        writeln!(
            writer,
            r#"    <edge id="e{edge_id}" source="{src}" target="{dst}"><data key="d_label">TESTED_BY</data></edge>"#
        )?;
        edge_id += 1;
    }

    writeln!(writer, r#"  </graph>"#)?;
    writeln!(writer, r#"</graphml>"#)?;

    Ok(())
}

/// Write JSON with `{"nodes": [...], "edges": [...]}` format.
///
/// Each node has `id`, `type` (Module/Symbol), and all relevant properties.
/// Each edge has `source`, `target`, and `label`.
pub fn export_json<W: Write>(backend: &dyn GraphBackend, writer: &mut W) -> Result<()> {
    // ── Collect nodes ──
    let modules = backend.raw_query("MATCH (m:Module) RETURN m.id, m.name, m.file, m.language")?;
    let symbols = backend.raw_query(
        "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.start_line, s.end_line, s.language, s.visibility, s.parent, s.docstring",
    )?;

    // ── Collect edges ──
    let contains =
        backend.raw_query("MATCH (m:Module)-[:CONTAINS]->(s:Symbol) RETURN m.id, s.id")?;
    let calls = backend.raw_query("MATCH (a:Symbol)-[:CALLS]->(b:Symbol) RETURN a.id, b.id")?;
    let inherits =
        backend.raw_query("MATCH (a:Symbol)-[:INHERITS]->(b:Symbol) RETURN a.id, b.id")?;
    let tested_by =
        backend.raw_query("MATCH (a:Symbol)-[:TESTED_BY]->(b:Symbol) RETURN a.id, b.id")?;

    // Build output using manual JSON to avoid adding serde_json as a dependency
    // (infigraph-core already has serde_json)
    writeln!(writer, "{{")?;
    writeln!(writer, "  \"nodes\": [")?;

    let total_nodes = modules.len() + symbols.len();
    let mut node_idx: usize = 0;

    for row in &modules {
        let comma = if node_idx + 1 < total_nodes { "," } else { "" };
        writeln!(
            writer,
            "    {{\"id\": \"{}\", \"type\": \"Module\", \"name\": \"{}\", \"file\": \"{}\", \"language\": \"{}\"}}{}",
            json_escape(&row[0]),
            json_escape(&row[1]),
            json_escape(&row[2]),
            json_escape(&row[3]),
            comma
        )?;
        node_idx += 1;
    }

    for row in &symbols {
        let comma = if node_idx + 1 < total_nodes { "," } else { "" };
        writeln!(
            writer,
            "    {{\"id\": \"{}\", \"type\": \"Symbol\", \"name\": \"{}\", \"kind\": \"{}\", \"file\": \"{}\", \"start_line\": {}, \"end_line\": {}, \"language\": \"{}\", \"visibility\": \"{}\", \"parent\": \"{}\", \"docstring\": \"{}\"}}{}",
            json_escape(&row[0]),
            json_escape(&row[1]),
            json_escape(&row[2]),
            json_escape(&row[3]),
            row[4],
            row[5],
            json_escape(&row[6]),
            json_escape(&row[7]),
            json_escape(&row[8]),
            json_escape(&row[9]),
            comma
        )?;
        node_idx += 1;
    }

    writeln!(writer, "  ],")?;
    writeln!(writer, "  \"edges\": [")?;

    let total_edges = contains.len() + calls.len() + inherits.len() + tested_by.len();
    let mut edge_idx: usize = 0;

    let edge_sets: &[(&str, &Vec<Vec<String>>)] = &[
        ("CONTAINS", &contains),
        ("CALLS", &calls),
        ("INHERITS", &inherits),
        ("TESTED_BY", &tested_by),
    ];

    for (label, edges) in edge_sets {
        for row in *edges {
            let comma = if edge_idx + 1 < total_edges { "," } else { "" };
            writeln!(
                writer,
                "    {{\"source\": \"{}\", \"target\": \"{}\", \"label\": \"{}\"}}{}",
                json_escape(&row[0]),
                json_escape(&row[1]),
                label,
                comma
            )?;
            edge_idx += 1;
        }
    }

    writeln!(writer, "  ]")?;
    writeln!(writer, "}}")?;

    Ok(())
}
