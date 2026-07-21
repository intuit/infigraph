use anyhow::{Context, Result};
use serde_json::Value;

use crate::tools::helpers::open_prism;

pub fn tool_ingest_structured(args: &Value) -> Result<String> {
    let prism = open_prism(args)?;
    let backend = prism.backend().context("not initialized")?;

    let schema_id = args.get("schema_id").and_then(|v| v.as_str());
    let data_file = args.get("data_file").and_then(|v| v.as_str());
    let inline_data = args.get("data").and_then(|v| v.as_array());

    let project_root = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(std::path::Path::new)
        .unwrap_or(std::path::Path::new("."));

    let schemas = infigraph_core::structured::discover_schemas(project_root)?;

    if schemas.is_empty() {
        return Ok("No structured schemas found. Create .toml files in .infigraph/structured-schemas/ or ~/.infigraph/structured-schemas/".to_string());
    }

    if schema_id.is_none() && data_file.is_none() && inline_data.is_none() {
        let mut out = String::from("Available schemas:\n\n");
        for (path, schema) in &schemas {
            let ddl = schema.schema.generate_ddl();
            out.push_str(&format!(
                "  {} — {} (table: {}, {} columns, {} edges)\n    Source: {}\n    DDL: {}\n\n",
                schema.schema.schema_id,
                schema.schema.name,
                schema.schema.node_table,
                schema.schema.columns.len(),
                schema.schema.edges.len(),
                path.display(),
                ddl.first().unwrap_or(&String::new()),
            ));
        }
        return Ok(out);
    }

    let sid = schema_id.context("missing 'schema_id'")?;
    let (_, schema) = schemas
        .iter()
        .find(|(_, s)| s.schema.schema_id == sid)
        .with_context(|| format!("schema '{}' not found", sid))?;

    let source_dir = args.get("source").and_then(|v| v.as_str());

    if let Some(dir) = source_dir {
        let path = std::path::Path::new(dir);
        let result = backend.ingest_structured_directory(&schema.schema, path)?;
        Ok(format!(
            "Ingested directory '{}' using schema '{}': {} nodes created, {} edges created",
            dir, sid, result.nodes_created, result.edges_created
        ))
    } else if let Some(file) = data_file {
        let path = std::path::Path::new(file);
        let result = backend.ingest_structured_file(&schema.schema, path)?;
        Ok(format!(
            "Ingested '{}' using schema '{}': {} nodes created, {} edges created",
            file, sid, result.nodes_created, result.edges_created
        ))
    } else if let Some(data) = inline_data {
        let data_vec: Vec<serde_json::Value> = data.clone();
        let result = backend.ingest_structured_data(&schema.schema, &data_vec)?;
        Ok(format!(
            "Ingested {} records using schema '{}': {} nodes created, {} edges created",
            data_vec.len(),
            sid,
            result.nodes_created,
            result.edges_created
        ))
    } else {
        Ok("Provide 'data_file' (path to .json/.yaml) or 'data' (inline JSON array)".to_string())
    }
}

pub fn tool_list_structured_schemas(args: &Value) -> Result<String> {
    let project_root = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(std::path::Path::new)
        .unwrap_or(std::path::Path::new("."));

    let schemas = infigraph_core::structured::discover_schemas(project_root)?;

    if schemas.is_empty() {
        return Ok("No structured schemas found. Create .toml files in .infigraph/structured-schemas/ or ~/.infigraph/structured-schemas/".to_string());
    }

    let mut out = String::from("Structured schemas:\n\n");
    for (path, schema) in &schemas {
        out.push_str(&format!(
            "  {} — {}\n    Table: {}, Columns: {}, Edges: {}\n    Source: {}\n\n",
            schema.schema.schema_id,
            schema.schema.name,
            schema.schema.node_table,
            schema.schema.columns.len(),
            schema.schema.edges.len(),
            path.display(),
        ));
    }
    Ok(out)
}
