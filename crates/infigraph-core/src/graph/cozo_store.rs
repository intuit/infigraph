use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use cozo::{DataValue, DbInstance, NamedRows, Num, ScriptMutability};

use std::collections::HashMap;

use super::queries::{
    format_skeleton, ApiSymbol, BranchInfo, CoverageRow, ExampleTest, FileDeps, HierarchyNode,
    ImpactRow, ReferenceRow, SkeletonSymbol, SymbolDetail, SymbolRow, TestContext, TestCoverage,
    TestTarget, TypeHierarchy,
};
use super::store::GraphStats;
use super::test_templates::test_templates_for;
use crate::model::{FileExtraction, RelationKind};

type Params = BTreeMap<String, DataValue>;

fn empty_params() -> Params {
    BTreeMap::new()
}

pub struct CozoStore {
    db: DbInstance,
}

impl CozoStore {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = DbInstance::new("sqlite", path.to_str().unwrap_or(""), Default::default())
            .map_err(|e| anyhow::anyhow!("failed to open cozo db: {e}"))?;
        let store = Self { db };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        for ddl in COZO_SCHEMA {
            match self
                .db
                .run_script(ddl, empty_params(), ScriptMutability::Mutable)
            {
                Ok(_) => {}
                Err(e) => {
                    let msg = format!("{e}");
                    if !msg.contains("already exists") && !msg.contains("conflicts") {
                        return Err(anyhow::anyhow!("schema error: {e}\n  DDL: {ddl}"));
                    }
                }
            }
        }
        for idx in COZO_INDICES {
            match self
                .db
                .run_script(idx, empty_params(), ScriptMutability::Mutable)
            {
                Ok(_) => {}
                Err(e) => {
                    let msg = format!("{e}");
                    if !msg.contains("already exists")
                        && !msg.contains("conflicts")
                        && !msg.contains("duplicate")
                    {
                        return Err(anyhow::anyhow!("index error: {e}\n  DDL: {idx}"));
                    }
                }
            }
        }
        Ok(())
    }

    fn run(&self, script: &str) -> Result<NamedRows> {
        self.db
            .run_script(script, empty_params(), ScriptMutability::Immutable)
            .map_err(|e| {
                anyhow::anyhow!(
                    "query failed: {e}\n  script: {}",
                    &script[..script.len().min(200)]
                )
            })
    }

    fn run_params(&self, script: &str, params: Params, mutable: bool) -> Result<NamedRows> {
        let m = if mutable {
            ScriptMutability::Mutable
        } else {
            ScriptMutability::Immutable
        };
        self.db.run_script(script, params, m).map_err(|e| {
            anyhow::anyhow!(
                "query failed: {e}\n  script: {}",
                &script[..script.len().min(200)]
            )
        })
    }

    // ── Read queries (match GraphQuery interface) ──────────────────────

    pub fn symbols_in_file(&self, file: &str) -> Result<Vec<SymbolRow>> {
        let mut params = empty_params();
        params.insert("file".into(), DataValue::Str(file.into()));
        let r = self.run_params(
            r#"?[id, name, kind, start_line, end_line] :=
                *defines{file_id: $file, symbol_id: id},
                *symbol{id, name, kind, start_line, end_line}
            :order start_line"#,
            params,
            false,
        )?;
        Ok(named_rows_to_symbol_rows(&r))
    }

    pub fn skeleton(&self, file: &str) -> Result<String> {
        let mut params = empty_params();
        params.insert("file".into(), DataValue::Str(file.into()));

        let r = self.run_params(
            r#"?[id, name, kind, start_line, complexity, parameters, return_type, visibility, parent] :=
                *symbol{id, name, kind, file, start_line, complexity, parameters, return_type, visibility, parent},
                file = $file
            :order start_line"#,
            params.clone(),
            false,
        )?;

        if r.rows.is_empty() {
            return Ok(format!(
                "No symbols found in '{file}'. File may not be indexed."
            ));
        }

        let ids: Vec<String> = r.rows.iter().map(|row| dv_str(&row[0])).collect();

        let mut fan_in: HashMap<String, usize> = HashMap::new();
        for id in &ids {
            let mut p = empty_params();
            p.insert("target".into(), DataValue::Str(id.clone().into()));
            let cr = self.run_params(
                r#"?[count(caller)] := *calls{caller, callee: $target}"#,
                p,
                false,
            )?;
            fan_in.insert(id.clone(), dv_u64(&cr) as usize);
        }

        let mut stmt_counts: HashMap<String, usize> = HashMap::new();
        let mut nesting: HashMap<String, u32> = HashMap::new();
        for id in &ids {
            let mut p = empty_params();
            p.insert("sym".into(), DataValue::Str(id.clone().into()));
            let sr = self.run_params(
                r#"?[count(stmt_id), max(depth)] :=
                    *has_statement{symbol_id: $sym, statement_id: stmt_id},
                    *statement{id: stmt_id, depth}"#,
                p,
                false,
            )?;
            if let Some(row) = sr.rows.first() {
                stmt_counts.insert(id.clone(), dv_u64_val(&row[0]) as usize);
                nesting.insert(id.clone(), dv_u32(&row[1]));
            }
        }

        let symbols: Vec<SkeletonSymbol> = r
            .rows
            .iter()
            .map(|row| {
                let id = dv_str(&row[0]);
                SkeletonSymbol {
                    fan_in: fan_in.get(&id).copied().unwrap_or(0),
                    stmt_count: stmt_counts.get(&id).copied().unwrap_or(0),
                    nesting: nesting.get(&id).copied().unwrap_or(0),
                    id,
                    name: dv_str(&row[1]),
                    kind: dv_str(&row[2]),
                    start_line: dv_str(&row[3]),
                    complexity: dv_u32(&row[4]),
                    params: dv_str(&row[5]),
                    return_type: dv_str(&row[6]),
                    visibility: dv_str(&row[7]),
                    parent: dv_str(&row[8]),
                }
            })
            .collect();

        Ok(format_skeleton(file, &symbols))
    }

    pub fn callers_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        let mut params = empty_params();
        params.insert("target".into(), DataValue::Str(symbol_id.into()));
        let r = self.run_params(
            r#"?[caller_id] := *calls{caller: caller_id, callee: $target}"#,
            params,
            false,
        )?;
        Ok(collect_strings(&r))
    }

    pub fn callees_of(&self, symbol_id: &str) -> Result<Vec<String>> {
        let mut params = empty_params();
        params.insert("source".into(), DataValue::Str(symbol_id.into()));
        let r = self.run_params(
            r#"?[callee_id] := *calls{caller: $source, callee: callee_id}"#,
            params,
            false,
        )?;
        Ok(collect_strings(&r))
    }

    pub fn find_symbol_by_id(&self, symbol_id: &str) -> Result<Option<SymbolDetail>> {
        let mut params = empty_params();
        params.insert("id".into(), DataValue::Str(symbol_id.into()));
        let r = self.run_params(
            r#"?[id, name, kind, file, start_line, end_line] :=
                id = $id,
                *symbol{id, name, kind, file, start_line, end_line}"#,
            params,
            false,
        )?;
        if let Some(row) = r.rows.first() {
            Ok(Some(row_to_symbol_detail(row)))
        } else {
            Ok(None)
        }
    }

    pub fn branches_of(&self, symbol_id: &str) -> Result<Vec<BranchInfo>> {
        let mut params = empty_params();
        params.insert("sym".into(), DataValue::Str(symbol_id.into()));
        let r = self.run_params(
            r#"?[kind, condition, start_line, depth] :=
                *has_statement{symbol_id: $sym, statement_id: st_id},
                *statement{id: st_id, kind, condition, start_line, depth}
            :order start_line"#,
            params,
            false,
        )?;
        Ok(r.rows
            .iter()
            .map(|row| BranchInfo {
                kind: dv_str(&row[0]),
                condition: dv_str(&row[1]),
                line: dv_u32(&row[2]),
                depth: dv_u32(&row[3]),
            })
            .collect())
    }

    pub fn transitive_impact(&self, symbol_id: &str, max_depth: u32) -> Result<Vec<ImpactRow>> {
        let mut params = empty_params();
        params.insert("target".into(), DataValue::Str(symbol_id.into()));
        // Unroll recursion: bind callee from previous layer first for index use
        let mut rules = String::new();
        rules.push_str("layer_1[caller] := *calls{caller, callee: $target}\n");
        for d in 2..=max_depth {
            rules.push_str(&format!(
                "layer_{d}[caller] := layer_{}[mid], *calls{{caller, callee: mid}}\n",
                d - 1
            ));
        }
        // Union all layers
        for d in 1..=max_depth {
            rules.push_str(&format!(
                "?[id, name, file, kind] := layer_{d}[id], *symbol{{id, name, file, kind}}\n"
            ));
        }
        let r = self.run_params(&rules, params, false)?;
        Ok(r.rows
            .iter()
            .map(|row| ImpactRow {
                id: dv_str(&row[0]),
                name: dv_str(&row[1]),
                file: dv_str(&row[2]),
                kind: dv_str(&row[3]),
            })
            .collect())
    }

    pub fn symbols_in_range(&self, file: &str, start: u32, end: u32) -> Result<Vec<SymbolDetail>> {
        let mut params = empty_params();
        params.insert("file".into(), DataValue::Str(file.into()));
        params.insert("start".into(), DataValue::from(start as i64));
        params.insert("end".into(), DataValue::from(end as i64));
        let r = self.run_params(
            r#"?[id, name, kind, file, start_line, end_line] :=
                *defines{file_id: $file, symbol_id: id},
                *symbol{id, name, kind, file, start_line, end_line},
                start_line <= $end, end_line >= $start
            :order start_line"#,
            params,
            false,
        )?;
        Ok(r.rows.iter().map(|row| row_to_symbol_detail(row)).collect())
    }

    pub fn find_all_references(&self, symbol_id: &str) -> Result<Vec<ReferenceRow>> {
        let mut params = empty_params();
        params.insert("target".into(), DataValue::Str(symbol_id.into()));
        let r = self.run_params(
            r#"?[caller_id, caller_name, file, start_line, target_id] :=
                *calls{caller: caller_id, callee: $target},
                *symbol{id: caller_id, name: caller_name, file, start_line},
                target_id = $target"#,
            params,
            false,
        )?;
        Ok(r.rows
            .iter()
            .map(|row| ReferenceRow {
                caller_id: dv_str(&row[0]),
                caller_name: dv_str(&row[1]),
                file: dv_str(&row[2]),
                line: dv_u32(&row[3]),
                target_id: dv_str(&row[4]),
            })
            .collect())
    }

    pub fn get_api_surface(&self) -> Result<Vec<ApiSymbol>> {
        let mut params = empty_params();
        params.insert("vis".into(), DataValue::Str("public".into()));
        params.insert("route".into(), DataValue::Str("Route".into()));
        let r = self.run_params(
            r#"?[id, name, kind, file, start_line, visibility, docstring] :=
                visibility = $vis,
                *symbol{id, name, kind, file, start_line, visibility, docstring}
            ?[id, name, kind, file, start_line, visibility, docstring] :=
                kind = $route,
                *symbol{id, name, kind, file, start_line, visibility, docstring}
            :order file, start_line"#,
            params,
            false,
        )?;
        Ok(r.rows
            .iter()
            .map(|row| ApiSymbol {
                id: dv_str(&row[0]),
                name: dv_str(&row[1]),
                kind: dv_str(&row[2]),
                file: dv_str(&row[3]),
                line: dv_u32(&row[4]),
                visibility: dv_str(&row[5]),
                docstring: dv_str(&row[6]),
            })
            .collect())
    }

    pub fn get_file_deps(&self, file: &str) -> Result<FileDeps> {
        let mut params = empty_params();
        params.insert("file".into(), DataValue::Str(file.into()));

        let r_out = self.run_params(
            r#"?[dep_file] := *imports{importer: $file, imported: dep_id},
                *module{id: dep_id, file: dep_file}"#,
            params.clone(),
            false,
        )?;
        let imports = collect_strings(&r_out);

        let r_in = self.run_params(
            r#"?[importer_file] := *imports{importer: imp_id, imported: $file},
                *module{id: imp_id, file: importer_file}"#,
            params,
            false,
        )?;
        let imported_by = collect_strings(&r_in);

        Ok(FileDeps {
            file: file.to_string(),
            imports,
            imported_by,
        })
    }

    pub fn get_type_hierarchy(&self, symbol_id: &str, max_depth: u32) -> Result<TypeHierarchy> {
        let mut params = empty_params();
        params.insert("root".into(), DataValue::Str(symbol_id.into()));

        // Ancestors: unrolled layers walking INHERITS upward
        let mut up_rules = String::new();
        up_rules.push_str("layer_1[parent] := *inherits{child: $root, parent}\n");
        for d in 2..=max_depth {
            up_rules.push_str(&format!(
                "layer_{d}[gp] := layer_{}[p], *inherits{{child: p, parent: gp}}\n",
                d - 1
            ));
        }
        for d in 1..=max_depth {
            up_rules.push_str(&format!(
                "?[id, name, kind, file] := layer_{d}[id], *symbol{{id, name, kind, file}}\n"
            ));
        }
        let r_up = self.run_params(&up_rules, params.clone(), false)?;
        let ancestors: Vec<HierarchyNode> = r_up
            .rows
            .iter()
            .map(|row| HierarchyNode {
                id: dv_str(&row[0]),
                name: dv_str(&row[1]),
                kind: dv_str(&row[2]),
                file: dv_str(&row[3]),
            })
            .collect();

        // Descendants: unrolled layers walking INHERITS downward
        let mut down_rules = String::new();
        down_rules.push_str("layer_1[child] := *inherits{child, parent: $root}\n");
        for d in 2..=max_depth {
            down_rules.push_str(&format!(
                "layer_{d}[gc] := layer_{}[p], *inherits{{child: gc, parent: p}}\n",
                d - 1
            ));
        }
        for d in 1..=max_depth {
            down_rules.push_str(&format!(
                "?[id, name, kind, file] := layer_{d}[id], *symbol{{id, name, kind, file}}\n"
            ));
        }
        let r_down = self.run_params(&down_rules, params.clone(), false)?;
        let descendants: Vec<HierarchyNode> = r_down
            .rows
            .iter()
            .map(|row| HierarchyNode {
                id: dv_str(&row[0]),
                name: dv_str(&row[1]),
                kind: dv_str(&row[2]),
                file: dv_str(&row[3]),
            })
            .collect();

        let root_detail = self.find_symbol_by_id(symbol_id)?;

        Ok(TypeHierarchy {
            root_id: symbol_id.to_string(),
            root_name: root_detail
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_default(),
            ancestors,
            descendants,
        })
    }

    pub fn get_test_coverage(&self) -> Result<TestCoverage> {
        // Query 1: all tested_by edges (small, indexed)
        let r_tested = self.run(r#"?[symbol_id, test_id] := *tested_by{symbol_id, test_id}"#)?;
        let mut tested_map: HashMap<String, String> = HashMap::new();
        for row in &r_tested.rows {
            tested_map.insert(dv_str(&row[0]), dv_str(&row[1]));
        }

        // Query 2: all symbols, filter testable kinds in Rust
        let r_syms = self.run(r#"?[id, name, kind, file] := *symbol{id, name, kind, file}"#)?;

        let mut covered = Vec::new();
        let mut uncovered = Vec::new();
        for row in &r_syms.rows {
            let kind = dv_str(&row[2]);
            if !is_testable_kind(&kind) {
                continue;
            }
            let id = dv_str(&row[0]);
            if let Some(test_id) = tested_map.get(&id) {
                covered.push(CoverageRow {
                    symbol_id: id,
                    symbol_name: dv_str(&row[1]),
                    kind,
                    file: dv_str(&row[3]),
                    test_id: Some(test_id.clone()),
                });
            } else {
                uncovered.push(CoverageRow {
                    symbol_id: id,
                    symbol_name: dv_str(&row[1]),
                    kind,
                    file: dv_str(&row[3]),
                    test_id: None,
                });
            }
        }

        let total = covered.len() + uncovered.len();
        let pct = (covered.len() * 100).checked_div(total).unwrap_or(0);

        Ok(TestCoverage {
            covered_count: covered.len(),
            uncovered_count: uncovered.len(),
            coverage_pct: pct,
            covered,
            uncovered,
        })
    }

    pub fn generate_test_context(
        &self,
        file_filter: Option<&str>,
        limit: usize,
        test_type: Option<&str>,
    ) -> Result<TestContext> {
        let framework = self.detect_test_framework()?;
        let example_test = self.find_example_test(file_filter)?;
        let templates = test_templates_for(&framework, test_type);

        // Get tested symbol IDs (small set, indexed)
        let r_tested = self.run(r#"?[symbol_id] := *tested_by{symbol_id, test_id: _}"#)?;
        let tested_ids: std::collections::HashSet<String> =
            r_tested.rows.iter().map(|row| dv_str(&row[0])).collect();

        // Get testable symbols via kind index (6 indexed lookups)
        let file_clause = if let Some(f) = file_filter {
            format!(r#", starts_with(file, "{}")"#, f.replace('"', "\\\""))
        } else {
            String::new()
        };

        let r = self.run(&format!(
            r#"?[id, name, kind, file, start_line, end_line, visibility, parameters, return_type, complexity] :=
                *symbol{{id, name, kind, file, start_line, end_line, visibility, parameters, return_type, complexity}}{file_clause}
            :order -complexity, file, start_line"#,
        ))?;

        // Filter testable + untested in Rust
        let mut targets: Vec<TestTarget> = r
            .rows
            .iter()
            .filter(|row| {
                let kind = dv_str(&row[2]);
                is_testable_kind(&kind) && !tested_ids.contains(&dv_str(&row[0]))
            })
            .map(|row| {
                let visibility = dv_str(&row[6]);
                let complexity = dv_u32(&row[9]);
                let vis_score: u32 = if visibility == "public" || visibility == "pub" {
                    10
                } else {
                    0
                };
                TestTarget {
                    symbol_id: dv_str(&row[0]),
                    name: dv_str(&row[1]),
                    kind: dv_str(&row[2]),
                    file: dv_str(&row[3]),
                    start_line: dv_u32(&row[4]),
                    end_line: dv_u32(&row[5]),
                    visibility,
                    parameters: dv_str(&row[7]),
                    return_type: dv_str(&row[8]),
                    complexity,
                    callers: Vec::new(),
                    callees: Vec::new(),
                    branches: Vec::new(),
                    priority_score: complexity * 5 + vis_score,
                }
            })
            .take(limit)
            .collect();

        // Batch fetch callers/callees/branches for all targets in one query each
        if !targets.is_empty() {
            let ids: Vec<&str> = targets.iter().map(|t| t.symbol_id.as_str()).collect();
            let callers_map = self.batch_callers(&ids)?;
            let callees_map = self.batch_callees(&ids)?;
            let mut branches_map = self.batch_branches(&ids)?;

            for t in &mut targets {
                t.callers = callers_map.get(&t.symbol_id).cloned().unwrap_or_default();
                t.callees = callees_map.get(&t.symbol_id).cloned().unwrap_or_default();
                t.branches = branches_map.remove(&t.symbol_id).unwrap_or_default();
                t.priority_score += t.callers.len() as u32 * 3;
            }
        }

        targets.sort_by_key(|t| std::cmp::Reverse(t.priority_score));

        Ok(TestContext {
            framework,
            example_test,
            targets,
            templates,
        })
    }

    fn detect_test_framework(&self) -> Result<String> {
        let r = self.run(
            r#"?[lang, count(lang)] := *symbol{kind: "Test", language: lang}
            :order -count(lang)
            :limit 1"#,
        );
        if let Ok(r) = r {
            if let Some(row) = r.rows.first() {
                let lang = dv_str(&row[0]);
                let fw = match lang.as_str() {
                    "go" => "go (go test)",
                    "rust" => "rust (cargo test)",
                    "python" => "python (unittest/pytest)",
                    "java" => "java (junit)",
                    "kotlin" => "kotlin (kotlin-test)",
                    "scala" => "scala (scalatest)",
                    "csharp" => "csharp (nunit/xunit)",
                    "javascript" | "typescript" => "javascript (jest/vitest)",
                    "ruby" => "ruby (rspec)",
                    "swift" => "swift (XCTest)",
                    "elixir" => "elixir (ExUnit)",
                    _ if !lang.is_empty() => return Ok(format!("{lang} (detected)")),
                    _ => "unknown",
                };
                if fw != "unknown" {
                    return Ok(fw.to_string());
                }
            }
        }
        Ok("unknown".to_string())
    }

    fn find_example_test(&self, file_filter: Option<&str>) -> Result<Option<ExampleTest>> {
        let file_clause = if let Some(f) = file_filter {
            format!(r#", starts_with(file, "{}")"#, f.replace('"', "\\\""))
        } else {
            String::new()
        };
        let r = self.run(&format!(
            r#"?[id, name, file, start_line, end_line] :=
                *symbol{{id, name, kind: "Test", file, start_line, end_line}}{file_clause}
            :limit 1"#,
        ))?;
        if let Some(row) = r.rows.first() {
            Ok(Some(ExampleTest {
                symbol_id: dv_str(&row[0]),
                name: dv_str(&row[1]),
                file: dv_str(&row[2]),
                start_line: dv_u32(&row[3]),
                end_line: dv_u32(&row[4]),
            }))
        } else {
            Ok(None)
        }
    }

    pub fn raw_query(&self, script: &str) -> Result<Vec<Vec<String>>> {
        let r = self.run(script)?;
        Ok(r.rows
            .iter()
            .map(|row| row.iter().map(dv_str).collect())
            .collect())
    }

    // ── Write methods (for migration) ─────────────────────────────────

    #[allow(clippy::type_complexity)]
    pub fn import_symbols(
        &self,
        rows: &[(
            String,
            String,
            String,
            String,
            i64,
            i64,
            String,
            String,
            String,
            String,
            String,
            i64,
            String,
            String,
        )],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec![
            "id".into(),
            "name".into(),
            "kind".into(),
            "file".into(),
            "start_line".into(),
            "end_line".into(),
            "signature_hash".into(),
            "language".into(),
            "visibility".into(),
            "parent".into(),
            "docstring".into(),
            "complexity".into(),
            "parameters".into(),
            "return_type".into(),
            "category".into(),
        ];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                    DataValue::Str(r.3.clone().into()),
                    DataValue::from(r.4),
                    DataValue::from(r.5),
                    DataValue::Str(r.6.clone().into()),
                    DataValue::Str(r.7.clone().into()),
                    DataValue::Str(r.8.clone().into()),
                    DataValue::Str(r.9.clone().into()),
                    DataValue::Str(r.10.clone().into()),
                    DataValue::from(r.11),
                    DataValue::Str(r.12.clone().into()),
                    DataValue::Str(r.13.clone().into()),
                    DataValue::Str("impl".into()),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("symbol".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import symbols: {e}"))
    }

    pub fn import_modules(
        &self,
        rows: &[(String, String, String, String, String, String)],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec![
            "id".into(),
            "name".into(),
            "file".into(),
            "language".into(),
            "content_hash".into(),
            "summary".into(),
        ];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                    DataValue::Str(r.3.clone().into()),
                    DataValue::Str(r.4.clone().into()),
                    DataValue::Str(r.5.clone().into()),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("module".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import modules: {e}"))
    }

    pub fn import_files(&self, rows: &[(String, String, String, String, i64)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec![
            "id".into(),
            "name".into(),
            "path".into(),
            "language".into(),
            "symbol_count".into(),
        ];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                    DataValue::Str(r.3.clone().into()),
                    DataValue::from(r.4),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("file".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import files: {e}"))
    }

    pub fn import_edges(&self, relation: &str, pairs: &[(String, String)]) -> Result<()> {
        if pairs.is_empty() {
            return Ok(());
        }
        let (col_a, col_b) = edge_columns(relation);
        if relation == "calls" {
            let headers = vec![col_a.to_string(), col_b.to_string(), "line".to_string()];
            let data_rows: Vec<Vec<DataValue>> = pairs
                .iter()
                .map(|(a, b)| {
                    vec![
                        DataValue::Str(a.clone().into()),
                        DataValue::Str(b.clone().into()),
                        DataValue::from(0i64),
                    ]
                })
                .collect();
            let named = NamedRows::new(headers, data_rows);
            let mut map = BTreeMap::new();
            map.insert(relation.to_string(), named);
            self.db
                .import_relations(map)
                .map_err(|e| anyhow::anyhow!("import {relation}: {e}"))
        } else {
            let headers = vec![col_a.to_string(), col_b.to_string()];
            let data_rows: Vec<Vec<DataValue>> = pairs
                .iter()
                .map(|(a, b)| {
                    vec![
                        DataValue::Str(a.clone().into()),
                        DataValue::Str(b.clone().into()),
                    ]
                })
                .collect();
            let named = NamedRows::new(headers, data_rows);
            let mut map = BTreeMap::new();
            map.insert(relation.to_string(), named);
            self.db
                .import_relations(map)
                .map_err(|e| anyhow::anyhow!("import {relation}: {e}"))
        }
    }

    pub fn import_calls_with_lines(&self, rows: &[(String, String, i64)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec!["caller".into(), "callee".into(), "line".into()];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::from(r.2),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("calls".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import calls: {e}"))
    }

    pub fn import_statements(
        &self,
        rows: &[(String, String, String, i64, i64, i64, String)],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec![
            "id".into(),
            "kind".into(),
            "condition".into(),
            "start_line".into(),
            "end_line".into(),
            "depth".into(),
            "parent_symbol".into(),
        ];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                    DataValue::from(r.3),
                    DataValue::from(r.4),
                    DataValue::from(r.5),
                    DataValue::Str(r.6.clone().into()),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("statement".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import statements: {e}"))
    }

    pub fn import_raw(
        &self,
        relation: &str,
        headers: Vec<String>,
        rows: Vec<Vec<DataValue>>,
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let named = NamedRows::new(headers, rows);
        let mut map = BTreeMap::new();
        map.insert(relation.to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import {relation}: {e}"))
    }

    pub fn import_folders(&self, rows: &[(String, String, String)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec!["id".into(), "name".into(), "path".into()];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("folder".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import folders: {e}"))
    }

    pub fn import_dependencies(
        &self,
        rows: &[(String, String, String, String, bool)],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec![
            "id".into(),
            "name".into(),
            "version".into(),
            "ecosystem".into(),
            "is_dev".into(),
        ];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                    DataValue::Str(r.3.clone().into()),
                    DataValue::Bool(r.4),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("dependency".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import dependencies: {e}"))
    }

    pub fn import_clusters(&self, rows: &[(String, String, String)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec!["id".into(), "name".into(), "description".into()];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("cluster".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import clusters: {e}"))
    }

    // ── Write methods (file upsert) ────────────────────────────────────

    pub fn upsert_file(&self, extraction: &FileExtraction) -> Result<()> {
        self.delete_file_data(&extraction.file)?;
        self.insert_file_data(extraction)?;
        self.invalidate_caches()
    }

    pub fn upsert_file_batch(&self, extraction: &FileExtraction) -> Result<()> {
        self.delete_file_data(&extraction.file)?;
        self.insert_file_data(extraction)
    }

    fn delete_file_data(&self, file: &str) -> Result<()> {
        let mut params = empty_params();
        params.insert("file".into(), DataValue::Str(file.into()));

        // Delete statements owned by symbols in this file
        let _ = self.run_params(
            r#"?[id] := *defines{file_id: $file, symbol_id: sym}, *has_statement{symbol_id: sym, statement_id: id}
            :rm statement {id}"#,
            params.clone(), true,
        );
        // Delete has_statement edges for symbols in this file
        let _ = self.run_params(
            r#"?[symbol_id, statement_id] := *defines{file_id: $file, symbol_id}, *has_statement{symbol_id, statement_id}
            :rm has_statement {symbol_id, statement_id}"#,
            params.clone(), true,
        );
        // Delete call edges where caller is in this file
        let _ = self.run_params(
            r#"?[caller, callee, line] := *defines{file_id: $file, symbol_id: caller}, *calls{caller, callee, line}
            :rm calls {caller, callee, line}"#,
            params.clone(), true,
        );
        // Delete other edges where source is in this file
        for (rel, col_a, col_b) in &[
            ("inherits", "child", "parent"),
            ("tested_by", "symbol_id", "test_id"),
            ("reads_rel", "reader", "target"),
            ("writes_rel", "writer", "target"),
            ("has_concern", "symbol_id", "concern_id"),
            ("has_config", "symbol_id", "config_id"),
            ("resolves_to", "source", "target"),
            ("taint_flow", "source", "target"),
        ] {
            let q = format!(
                "?[{col_a}, {col_b}] := *defines{{file_id: $file, symbol_id: {col_a}}}, *{rel}{{{col_a}, {col_b}}}
                :rm {rel} {{{col_a}, {col_b}}}"
            );
            let _ = self.run_params(&q, params.clone(), true);
        }
        // Delete contains edges (module -> symbols)
        let _ = self.run_params(
            r#"?[module_id, symbol_id] := *contains{module_id: $file, symbol_id}
            :rm contains {module_id, symbol_id}"#,
            params.clone(),
            true,
        );
        // Delete imports edges from this module
        let _ = self.run_params(
            r#"?[importer, imported] := *imports{importer: $file, imported}
            :rm imports {importer, imported}"#,
            params.clone(),
            true,
        );
        // Delete defines edges
        let _ = self.run_params(
            r#"?[file_id, symbol_id] := *defines{file_id: $file, symbol_id}
            :rm defines {file_id, symbol_id}"#,
            params.clone(),
            true,
        );
        // Delete symbols for this file
        let _ = self.run_params(
            r#"?[id] := *symbol{id, file: $file}
            :rm symbol {id}"#,
            params.clone(),
            true,
        );
        // Delete module
        let _ = self.run_params(
            r#"?[id] := id = $file
            :rm module {id}"#,
            params.clone(),
            true,
        );
        // Delete file node
        let _ = self.run_params(
            r#"?[id] := id = $file
            :rm file {id}"#,
            params,
            true,
        );
        Ok(())
    }

    fn insert_file_data(&self, extraction: &FileExtraction) -> Result<()> {
        let module_id = &extraction.file;
        let module_name = extraction
            .file
            .rsplit_once('/')
            .map(|(_, f)| f)
            .unwrap_or(&extraction.file);
        let file_name = module_name;

        // Insert module
        self.import_modules(&[(
            module_id.clone(),
            module_name.to_string(),
            extraction.file.clone(),
            extraction.language.clone(),
            extraction.content_hash.clone(),
            String::new(),
        )])?;

        // Insert file node
        self.import_files(&[(
            extraction.file.clone(),
            file_name.to_string(),
            extraction.file.clone(),
            extraction.language.clone(),
            extraction.symbols.len() as i64,
        )])?;

        // Insert symbols
        if !extraction.symbols.is_empty() {
            let sym_rows: Vec<_> = extraction
                .symbols
                .iter()
                .map(|sym| {
                    (
                        sym.id.clone(),
                        sym.name.clone(),
                        sym.kind.as_str().to_string(),
                        extraction.file.clone(),
                        sym.span.start_line as i64,
                        sym.span.end_line as i64,
                        sym.signature_hash.clone(),
                        sym.language.clone(),
                        sym.visibility.clone().unwrap_or_default(),
                        sym.parent.clone().unwrap_or_default(),
                        sym.docstring.clone().unwrap_or_default(),
                        sym.complexity as i64,
                        sym.parameters.clone().unwrap_or_default(),
                        sym.return_type.clone().unwrap_or_default(),
                    )
                })
                .collect();
            self.import_symbols(&sym_rows)?;

            // CONTAINS edges: module -> symbols
            let contains: Vec<_> = extraction
                .symbols
                .iter()
                .map(|s| (module_id.clone(), s.id.clone()))
                .collect();
            self.import_edges("contains", &contains)?;

            // DEFINES edges: file -> symbols
            let defines: Vec<_> = extraction
                .symbols
                .iter()
                .map(|s| (extraction.file.clone(), s.id.clone()))
                .collect();
            self.import_edges("defines", &defines)?;
        }

        // Insert relations grouped by type
        let mut calls_rows: Vec<(String, String, i64)> = Vec::new();
        let mut inherits_pairs: Vec<(String, String)> = Vec::new();
        let mut tested_by_pairs: Vec<(String, String)> = Vec::new();
        let mut imports_pairs: Vec<(String, String)> = Vec::new();
        let mut reads_pairs: Vec<(String, String)> = Vec::new();
        let mut writes_pairs: Vec<(String, String)> = Vec::new();
        let mut custom_pairs: HashMap<String, Vec<(String, String)>> = HashMap::new();

        for rel in &extraction.relations {
            let line = rel.span.as_ref().map(|s| s.start_line as i64).unwrap_or(0);
            match &rel.kind {
                RelationKind::Calls | RelationKind::CalledBy => {
                    calls_rows.push((rel.source_id.clone(), rel.target_id.clone(), line));
                }
                RelationKind::Inherits | RelationKind::InheritedBy => {
                    inherits_pairs.push((rel.source_id.clone(), rel.target_id.clone()));
                }
                RelationKind::TestedBy | RelationKind::Tests => {
                    tested_by_pairs.push((rel.source_id.clone(), rel.target_id.clone()));
                }
                RelationKind::Imports | RelationKind::ImportedBy => {
                    imports_pairs.push((rel.source_id.clone(), rel.target_id.clone()));
                }
                RelationKind::Reads => {
                    reads_pairs.push((rel.source_id.clone(), rel.target_id.clone()));
                }
                RelationKind::Writes => {
                    writes_pairs.push((rel.source_id.clone(), rel.target_id.clone()));
                }
                RelationKind::Custom(edge_name) => {
                    custom_pairs
                        .entry(edge_name.clone())
                        .or_default()
                        .push((rel.source_id.clone(), rel.target_id.clone()));
                }
                _ => {}
            }
        }

        self.import_calls_with_lines(&calls_rows)?;
        self.import_edges("inherits", &inherits_pairs)?;
        self.import_edges("tested_by", &tested_by_pairs)?;
        self.import_edges("imports", &imports_pairs)?;
        self.import_edges("reads_rel", &reads_pairs)?;
        self.import_edges("writes_rel", &writes_pairs)?;

        for (edge_name, pairs) in &custom_pairs {
            let lower = edge_name.to_lowercase();
            let ddl = format!(":create {lower} {{source: String, target: String}}");
            let _ = self.create_custom_edge(&ddl);
            self.import_edges(&lower, pairs)?;
        }

        // Insert statements + HAS_STATEMENT edges
        if !extraction.statements.is_empty() {
            let stmt_rows: Vec<_> = extraction
                .statements
                .iter()
                .map(|s| {
                    (
                        s.id.clone(),
                        s.kind.as_str().to_string(),
                        s.condition.clone(),
                        s.start_line as i64,
                        s.end_line as i64,
                        s.depth as i64,
                        s.parent_symbol.clone(),
                    )
                })
                .collect();
            self.import_statements(&stmt_rows)?;

            let has_stmt: Vec<_> = extraction
                .statements
                .iter()
                .map(|s| (s.parent_symbol.clone(), s.id.clone()))
                .collect();
            self.import_edges("has_statement", &has_stmt)?;
        }

        Ok(())
    }

    pub fn refresh_materialized(&self) -> Result<()> {
        self.refresh_meta()?;
        self.refresh_testable()
    }

    fn invalidate_caches(&self) -> Result<()> {
        let _ = self.run_params(
            "?[key, val] <- []\n:replace meta_cache {key: String => val: Int}",
            empty_params(),
            true,
        );
        let _ = self.run_params(
            "?[id] <- []\n:replace testable_cache {id: String}",
            empty_params(),
            true,
        );
        Ok(())
    }

    fn batch_callers(&self, ids: &[&str]) -> Result<HashMap<String, Vec<String>>> {
        let vals: Vec<String> = ids
            .iter()
            .map(|id| format!("[\"{}\"]", id.replace('"', "\\\"")))
            .collect();
        let script = format!(
            "targets[id] <- [{}]\n?[callee, caller] := targets[callee], *calls{{caller, callee}}",
            vals.join(", ")
        );
        let r = self.run(&script)?;
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for row in &r.rows {
            map.entry(dv_str(&row[0]))
                .or_default()
                .push(dv_str(&row[1]));
        }
        Ok(map)
    }

    fn batch_callees(&self, ids: &[&str]) -> Result<HashMap<String, Vec<String>>> {
        let vals: Vec<String> = ids
            .iter()
            .map(|id| format!("[\"{}\"]", id.replace('"', "\\\"")))
            .collect();
        let script = format!(
            "targets[id] <- [{}]\n?[caller, callee] := targets[caller], *calls{{caller, callee}}",
            vals.join(", ")
        );
        let r = self.run(&script)?;
        let mut map: HashMap<String, Vec<String>> = HashMap::new();
        for row in &r.rows {
            map.entry(dv_str(&row[0]))
                .or_default()
                .push(dv_str(&row[1]));
        }
        Ok(map)
    }

    fn batch_branches(&self, ids: &[&str]) -> Result<HashMap<String, Vec<BranchInfo>>> {
        let vals: Vec<String> = ids
            .iter()
            .map(|id| format!("[\"{}\"]", id.replace('"', "\\\"")))
            .collect();
        let script = format!(
            "targets[id] <- [{}]\n?[sym, stmt_kind, condition, start_line, depth] := targets[sym], *has_statement{{symbol_id: sym, statement_id: sid}}, *statement{{id: sid, kind: stmt_kind, condition, start_line, depth}}",
            vals.join(", ")
        );
        let r = self.run(&script)?;
        let mut map: HashMap<String, Vec<BranchInfo>> = HashMap::new();
        for row in &r.rows {
            map.entry(dv_str(&row[0])).or_default().push(BranchInfo {
                kind: dv_str(&row[1]),
                condition: dv_str(&row[2]),
                line: dv_u32(&row[3]),
                depth: dv_u32(&row[4]),
            });
        }
        Ok(map)
    }

    fn refresh_meta(&self) -> Result<()> {
        let counts: &[(&str, &str)] = &[
            ("symbols", "?[count(id)] := *symbol{id}"),
            ("modules", "?[count(id)] := *module{id}"),
            ("files", "?[count(id)] := *file{id}"),
            ("folders", "?[count(id)] := *folder{id}"),
            ("calls", "?[count(caller)] := *calls{caller}"),
            ("inherits", "?[count(child)] := *inherits{child}"),
            ("contains", "?[count(module_id)] := *contains{module_id}"),
        ];
        let mut rows = Vec::new();
        for (key, query) in counts {
            let r = self.run(query)?;
            let val = dv_u64(&r) as i64;
            rows.push(format!("[\"{key}\", {val}]"));
        }
        let script = format!(
            "?[key, val] <- [{}]\n:replace meta_cache {{key: String => val: Int}}",
            rows.join(", ")
        );
        self.run_params(&script, empty_params(), true)?;
        Ok(())
    }

    fn refresh_testable(&self) -> Result<()> {
        self.run_params(
            r#"?[id] := *symbol{id, kind: "Function"}
            ?[id] := *symbol{id, kind: "Method"}
            ?[id] := *symbol{id, kind: "Class"}
            ?[id] := *symbol{id, kind: "Struct"}
            ?[id] := *symbol{id, kind: "Trait"}
            ?[id] := *symbol{id, kind: "Interface"}
            :replace testable_cache {id: String}"#,
            empty_params(),
            true,
        )?;
        Ok(())
    }

    pub fn create_custom_edge(&self, ddl: &str) -> Result<()> {
        match self
            .db
            .run_script(ddl, empty_params(), ScriptMutability::Mutable)
        {
            Ok(_) => Ok(()),
            Err(e) => {
                let msg = format!("{e}");
                if msg.contains("already exists") || msg.contains("conflicts") {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("create custom edge: {e}"))
                }
            }
        }
    }

    pub fn stats(&self) -> Result<GraphStats> {
        let r = self.run(r#"?[key, val] := *meta_cache{key, val}"#);
        if let Ok(r) = &r {
            if !r.rows.is_empty() {
                let m: HashMap<String, u64> = r
                    .rows
                    .iter()
                    .map(|row| (dv_str(&row[0]), dv_u64_val(&row[1])))
                    .collect();
                return Ok(GraphStats {
                    symbols: *m.get("symbols").unwrap_or(&0),
                    modules: *m.get("modules").unwrap_or(&0),
                    files: *m.get("files").unwrap_or(&0),
                    folders: *m.get("folders").unwrap_or(&0),
                    calls: *m.get("calls").unwrap_or(&0),
                    inherits: *m.get("inherits").unwrap_or(&0),
                    contains: *m.get("contains").unwrap_or(&0),
                });
            }
        }
        // Fallback: compute fresh and cache
        self.refresh_meta()?;
        self.stats()
    }

    // ── Parity methods (match GraphStore interface) ─────────────────────

    pub fn get_file_hashes(&self) -> Result<HashMap<String, String>> {
        let r = self.run(r#"?[file, content_hash] := *module{file, content_hash}"#)?;
        let mut map = HashMap::new();
        for row in &r.rows {
            map.insert(dv_str(&row[0]), dv_str(&row[1]));
        }
        Ok(map)
    }

    pub fn get_all_symbols(&self) -> Result<Vec<(String, String, String, String)>> {
        let r = self.run(r#"?[name, id, file, kind] := *symbol{id, name, file, kind}"#)?;
        Ok(r.rows
            .iter()
            .map(|row| {
                (
                    dv_str(&row[0]),
                    dv_str(&row[1]),
                    dv_str(&row[2]),
                    dv_str(&row[3]),
                )
            })
            .collect())
    }

    pub fn remove_file(&self, file: &str) -> Result<()> {
        self.delete_file_data(file)
    }

    pub fn upsert_all_bulk(&self, extractions: &[FileExtraction]) -> Result<()> {
        for e in extractions {
            self.upsert_file_batch(e)?;
        }
        self.invalidate_caches()
    }

    pub fn derive_tested_by_edges(&self) -> Result<usize> {
        let _ = self.run_params(
            r#"?[symbol_id, test_id] := *tested_by{symbol_id, test_id}
            :rm tested_by {symbol_id, test_id}"#,
            empty_params(),
            true,
        );
        self.run_params(
            r#"?[symbol_id, test_id] := *calls{caller: test_id, callee: symbol_id},
                *symbol{id: test_id, kind: "Test"},
                *symbol{id: symbol_id, kind},
                kind != "Test"
            :put tested_by {symbol_id, test_id}"#,
            empty_params(),
            true,
        )?;
        let r = self.run(r#"?[count(symbol_id)] := *tested_by{symbol_id}"#)?;
        Ok(dv_u64(&r) as usize)
    }

    pub fn cross_cutting_for(&self, symbol_id: &str) -> Result<Vec<(String, String)>> {
        let mut params = empty_params();
        params.insert("sym".into(), DataValue::Str(symbol_id.into()));
        let r = self.run_params(
            r#"?[kind, detail] := *has_concern{symbol_id: $sym, concern_id: cid}, *concern{id: cid, kind, detail}"#,
            params, false,
        )?;
        Ok(r.rows
            .iter()
            .map(|row| (dv_str(&row[0]), dv_str(&row[1])))
            .collect())
    }

    pub fn upsert_folders_bulk(&self, file_paths: &[&str]) -> Result<()> {
        let mut all_folders: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for file_path in file_paths {
            let parts: Vec<&str> = file_path.rsplitn(2, '/').collect();
            if parts.len() < 2 {
                continue;
            }
            let dir_path = parts[1];
            let segments: Vec<&str> = dir_path.split('/').collect();
            for i in 0..segments.len() {
                all_folders.insert(segments[..=i].join("/"));
            }
        }
        if all_folders.is_empty() {
            return Ok(());
        }

        let folder_rows: Vec<(String, String, String)> = all_folders
            .iter()
            .map(|fp| {
                let name = fp.rsplit_once('/').map(|(_, n)| n).unwrap_or(fp.as_str());
                (fp.clone(), name.to_string(), fp.clone())
            })
            .collect();
        self.import_folders(&folder_rows)?;

        let cf_pairs: Vec<(String, String)> = all_folders
            .iter()
            .filter_map(|child| {
                child
                    .rsplit_once('/')
                    .map(|(p, _)| p)
                    .and_then(|parent_path| {
                        if all_folders.contains(parent_path) {
                            Some((parent_path.to_string(), child.clone()))
                        } else {
                            None
                        }
                    })
            })
            .collect();
        self.import_edges("contains_folder", &cf_pairs)?;

        let cfile_pairs: Vec<(String, String)> = file_paths
            .iter()
            .filter_map(|fp| {
                let parts: Vec<&str> = fp.rsplitn(2, '/').collect();
                if parts.len() < 2 {
                    return None;
                }
                Some((parts[1].to_string(), fp.to_string()))
            })
            .collect();
        self.import_edges("contains_file", &cfile_pairs)?;

        Ok(())
    }

    pub fn import_concerns(&self, rows: &[(String, String, String)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec!["id".into(), "kind".into(), "detail".into()];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("concern".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import concerns: {e}"))
    }

    pub fn import_config_bindings(
        &self,
        rows: &[(String, String, String, String, String, String)],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec![
            "id".into(),
            "kind".into(),
            "key".into(),
            "value".into(),
            "profile".into(),
            "source_file".into(),
        ];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                    DataValue::Str(r.3.clone().into()),
                    DataValue::Str(r.4.clone().into()),
                    DataValue::Str(r.5.clone().into()),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("config_binding".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import config_bindings: {e}"))
    }

    pub fn import_taint_flows(
        &self,
        rows: &[(String, String, String, String, String)],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec![
            "source".into(),
            "target".into(),
            "source_kind".into(),
            "sink_kind".into(),
            "path".into(),
        ];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                    DataValue::Str(r.3.clone().into()),
                    DataValue::Str(r.4.clone().into()),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("taint_flow".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import taint_flows: {e}"))
    }

    pub fn import_resolves_to(&self, rows: &[(String, String, String, String)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let headers = vec![
            "source".into(),
            "target".into(),
            "mechanism".into(),
            "config_source".into(),
        ];
        let data_rows: Vec<Vec<DataValue>> = rows
            .iter()
            .map(|r| {
                vec![
                    DataValue::Str(r.0.clone().into()),
                    DataValue::Str(r.1.clone().into()),
                    DataValue::Str(r.2.clone().into()),
                    DataValue::Str(r.3.clone().into()),
                ]
            })
            .collect();
        let named = NamedRows::new(headers, data_rows);
        let mut map = BTreeMap::new();
        map.insert("resolves_to".to_string(), named);
        self.db
            .import_relations(map)
            .map_err(|e| anyhow::anyhow!("import resolves_to: {e}"))
    }

    pub fn relation_counts(&self) -> Result<BTreeMap<String, u64>> {
        let relations = [
            ("symbol", "id"),
            ("module", "id"),
            ("cluster", "id"),
            ("file", "id"),
            ("folder", "id"),
            ("dependency", "id"),
            ("statement", "id"),
            ("calls", "caller"),
            ("depends_on", "module_id"),
            ("imports", "importer"),
            ("contains", "module_id"),
            ("inherits", "child"),
            ("tested_by", "symbol_id"),
            ("reads_rel", "reader"),
            ("writes_rel", "writer"),
            ("member_of", "symbol_id"),
            ("similar_to", "symbol_a"),
            ("bridge_to", "source"),
            ("contains_file", "folder_id"),
            ("contains_folder", "parent_id"),
            ("defines", "file_id"),
            ("calls_service", "caller"),
            ("has_statement", "symbol_id"),
            ("concern", "id"),
            ("has_concern", "symbol_id"),
            ("config_binding", "id"),
            ("has_config", "symbol_id"),
            ("resolves_to", "source"),
            ("taint_flow", "source"),
        ];
        let mut counts = BTreeMap::new();
        for (rel, col) in &relations {
            let q = format!("?[count({col})] := *{rel}{{{col}}}");
            let r = self.run(&q)?;
            counts.insert(rel.to_string(), dv_u64(&r));
        }
        Ok(counts)
    }
}

// ── Schema ────────────────────────────────────────────────────────────

pub fn cozo_schema_ddl() -> Vec<&'static str> {
    COZO_SCHEMA.to_vec()
}

const COZO_SCHEMA: &[&str] = &[
    ":create symbol {id: String => name: String, kind: String, file: String, start_line: Int, end_line: Int, signature_hash: String default \"\", language: String default \"\", visibility: String default \"\", parent: String default \"\", docstring: String default \"\", complexity: Int default 1, parameters: String default \"\", return_type: String default \"\", category: String default \"impl\"}",
    ":create module {id: String => name: String, file: String, language: String, content_hash: String default \"\", summary: String default \"\"}",
    ":create cluster {id: String => name: String, description: String default \"\"}",
    ":create file {id: String => name: String, path: String, language: String, symbol_count: Int default 0}",
    ":create folder {id: String => name: String, path: String}",
    ":create dependency {id: String => name: String, version: String default \"\", ecosystem: String default \"\", is_dev: Bool default false}",
    ":create statement {id: String => kind: String, condition: String default \"\", start_line: Int default 0, end_line: Int default 0, depth: Int default 0, parent_symbol: String default \"\"}",
    ":create calls {caller: String, callee: String, line: Int default 0}",
    ":create depends_on {module_id: String, dep_id: String, is_dev: Bool default false}",
    ":create imports {importer: String, imported: String}",
    ":create contains {module_id: String, symbol_id: String}",
    ":create inherits {child: String, parent: String}",
    ":create tested_by {symbol_id: String, test_id: String}",
    ":create reads_rel {reader: String, target: String}",
    ":create writes_rel {writer: String, target: String}",
    ":create member_of {symbol_id: String, cluster_id: String}",
    ":create similar_to {symbol_a: String, symbol_b: String, score: Float default 0.0}",
    ":create bridge_to {source: String, target: String, bridge_kind: String default \"\", detail: String default \"\"}",
    ":create contains_file {folder_id: String, file_id: String}",
    ":create contains_folder {parent_id: String, child_id: String}",
    ":create defines {file_id: String, symbol_id: String}",
    ":create calls_service {caller: String, target: String, method: String default \"\", path: String default \"\", target_service: String default \"\", protocol: String default \"\", qualifier: String default \"\"}",
    ":create has_statement {symbol_id: String, statement_id: String}",
    ":create concern {id: String => kind: String, detail: String default \"\"}",
    ":create has_concern {symbol_id: String, concern_id: String}",
    ":create config_binding {id: String => kind: String, key: String, value: String default \"\", profile: String default \"\", source_file: String default \"\"}",
    ":create has_config {symbol_id: String, config_id: String}",
    ":create resolves_to {source: String, target: String, mechanism: String default \"\", config_source: String default \"\"}",
    ":create taint_flow {source: String, target: String, source_kind: String default \"\", sink_kind: String default \"\", path: String default \"\"}",
    // AIF3X-331 #16: parity entries for the Kuzu INJECTS_DEPENDENCY/
    // REGISTERS_MIDDLEWARE rel tables (schema_parity.rs requires every Kuzu
    // rel table to have a Cozo equivalent). Cozo isn't an active GraphBackend
    // implementation (only Kuzu/Neo4j are — see backend.rs), so this is
    // schema-shape parity only, not a functional query path.
    ":create injects_dependency {source: String, target: String}",
    ":create registers_middleware {source: String, target: String}",
    // Parity entry for the Kuzu ExternalRef node table / EXTERNAL_CALL rel
    // table (see schema.rs's ExternalRef comment for what these represent —
    // a call whose receiver resolves to a real type with no local Symbol).
    // Same schema-shape-only parity as injects_dependency/registers_middleware
    // above: Cozo isn't an active GraphBackend, so this exists purely to
    // satisfy schema_parity.rs, not a functional query path.
    ":create externalref {id: String => qualifier: String, method: String}",
    ":create external_call {source: String, target: String}",
    // Materialized helpers for fast aggregation
    ":create meta_cache {key: String => val: Int}",
    ":create testable_cache {id: String}",
];

const COZO_INDICES: &[&str] = &[
    // Reverse lookups on edges (PK orders by first column; these index the second)
    "::index create calls:calls_by_callee {callee}",
    "::index create inherits:inherits_by_parent {parent}",
    "::index create tested_by:tested_by_test {test_id}",
    "::index create defines:defines_by_symbol {symbol_id}",
    "::index create contains:contains_by_symbol {symbol_id}",
    "::index create imports:imports_by_imported {imported}",
    "::index create has_statement:has_stmt_by_stmt {statement_id}",
    "::index create reads_rel:reads_by_target {target}",
    "::index create writes_rel:writes_by_target {target}",
    "::index create similar_to:similar_by_b {symbol_b}",
    "::index create bridge_to:bridge_by_target {target}",
    "::index create contains_file:contains_file_by_file {file_id}",
    "::index create contains_folder:contains_folder_by_child {child_id}",
    "::index create calls_service:calls_svc_by_target {target}",
    "::index create member_of:member_by_cluster {cluster_id}",
    "::index create has_concern:has_concern_by_concern {concern_id}",
    "::index create has_config:has_config_by_config {config_id}",
    "::index create resolves_to:resolves_to_by_target {target}",
    "::index create taint_flow:taint_flow_by_target {target}",
    // Symbol column indices for filtered scans
    "::index create symbol:symbol_by_file {file}",
    "::index create symbol:symbol_by_kind {kind}",
    "::index create symbol:symbol_by_visibility {visibility}",
];

fn edge_columns(relation: &str) -> (&'static str, &'static str) {
    match relation {
        "calls" => ("caller", "callee"),
        "inherits" => ("child", "parent"),
        "tested_by" => ("symbol_id", "test_id"),
        "contains" => ("module_id", "symbol_id"),
        "defines" => ("file_id", "symbol_id"),
        "imports" => ("importer", "imported"),
        "has_statement" => ("symbol_id", "statement_id"),
        "reads_rel" => ("reader", "target"),
        "writes_rel" => ("writer", "target"),
        "has_concern" => ("symbol_id", "concern_id"),
        "has_config" => ("symbol_id", "config_id"),
        "resolves_to" => ("source", "target"),
        "taint_flow" => ("source", "target"),
        _ => ("source", "target"),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

fn dv_str(v: &DataValue) -> String {
    match v {
        DataValue::Str(s) => s.to_string(),
        DataValue::Null => String::new(),
        other => format!("{other:?}"),
    }
}

fn dv_u32(v: &DataValue) -> u32 {
    match v {
        DataValue::Num(Num::Int(i)) => *i as u32,
        DataValue::Num(Num::Float(f)) => *f as u32,
        _ => 0,
    }
}

fn dv_u64(r: &NamedRows) -> u64 {
    r.rows
        .first()
        .and_then(|row| row.first())
        .map(dv_u64_val)
        .unwrap_or(0)
}

fn dv_u64_val(v: &DataValue) -> u64 {
    match v {
        DataValue::Num(Num::Int(i)) => *i as u64,
        DataValue::Num(Num::Float(f)) => *f as u64,
        _ => 0,
    }
}

fn is_testable_kind(kind: &str) -> bool {
    matches!(
        kind,
        "Function" | "Method" | "Class" | "Struct" | "Trait" | "Interface"
    )
}

fn collect_strings(r: &NamedRows) -> Vec<String> {
    r.rows
        .iter()
        .filter_map(|row| row.first().map(dv_str))
        .collect()
}

fn named_rows_to_symbol_rows(r: &NamedRows) -> Vec<SymbolRow> {
    r.rows
        .iter()
        .map(|row| SymbolRow {
            id: dv_str(&row[0]),
            name: dv_str(&row[1]),
            kind: dv_str(&row[2]),
            start_line: dv_u32(&row[3]),
            end_line: dv_u32(&row[4]),
        })
        .collect()
}

fn row_to_symbol_detail(row: &[DataValue]) -> SymbolDetail {
    SymbolDetail {
        id: dv_str(&row[0]),
        name: dv_str(&row[1]),
        kind: dv_str(&row[2]),
        file: dv_str(&row[3]),
        start_line: dv_u32(&row[4]),
        end_line: dv_u32(&row[5]),
    }
}
