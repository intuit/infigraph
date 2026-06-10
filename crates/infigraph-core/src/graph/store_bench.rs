use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use arrow::array::{Int64Array, StringArray};
use arrow::datatypes::DataType;

use super::parquet_loader;
use super::store::GraphStore;
use super::store_util::{escape, fwd_slash_path};

impl GraphStore {
    /// Test: DELETE + COPY FROM parquet produces identical data to MERGE/UNWIND.
    /// Covers edge cases: <>, quotes, unicode, empty strings, backslashes, newlines.
    pub fn test_parquet_quality(&self) -> Result<()> {
        let conn = self.connection()?;

        let full_schema = "CREATE NODE TABLE %TABLE%(id STRING, name STRING, kind STRING, file STRING, start_line INT64, end_line INT64, signature_hash STRING, language STRING, visibility STRING, parent STRING, docstring STRING, complexity INT64, PRIMARY KEY(id))";

        // Edge case test data -- every known problematic pattern
        let long_doc = "A".repeat(10000);
        #[allow(clippy::type_complexity)]
        let test_rows: Vec<(&str, &str, &str, &str, i64, i64, &str, &str, &str, &str, &str, i64)> = vec![
            ("t1", "normal_func", "Function", "src/main.rs", 1, 10, "abc", "rust", "public", "", "Normal docstring", 3),
            ("t2", "angle_brackets", "Function", "src/lib.rs", 5, 20, "def", "java", "", "", "Returns List<String> from <code>parse</code>", 1),
            ("t3", "flask_route", "Function", "app.py", 2, 8, "ghi", "python", "public", "", "@app.route(\"/api/users/<int:id>\", methods=[\"GET\"])", 2),
            ("t4", "regex_group", "Function", "src/re.py", 10, 50, "jkl", "python", "", "", "(?P<query>.+)/$", 5),
            ("t5", "html_javadoc", "Method", "Foo.java", 3, 15, "mno", "java", "public", "Foo", "/** Wraps <p>text</p> in {@link List<T>} */", 4),
            ("t6", "double_quotes", "Function", "bar.rs", 1, 5, "pqr", "rust", "", "", "Returns \"hello world\" and \"goodbye\"", 1),
            ("t7", "single_quotes", "Function", "baz.py", 1, 5, "stu", "python", "", "", "It's a test with 'single' quotes", 1),
            ("t8", "backslashes", "Function", "esc.rs", 1, 5, "vwx", "rust", "", "", "Path is C:\\Users\\test\\file.txt", 1),
            ("t09", "unicode", "Class", "uni.py", 1, 5, "yza", "python", "", "Parent", "Ünïcödé: 日本語テスト 🚀", 0),
            ("t10", "empty_all", "Variable", "e.rs", 0, 0, "", "", "", "", "", 0),
            ("t11", "tab_content", "Function", "tab.rs", 1, 5, "tab", "rust", "", "", "col1\tcol2\tcol3", 1),
            ("t12", "newline_content", "Function", "nl.rs", 1, 5, "nln", "rust", "", "", "line1\nline2\nline3", 1),
            ("t13", "mixed_evil", "Function", "evil.java", 1, 99, "evil", "java", "public", "", "/** @param <T extends Comparable<? super T>> \\n uses 'single' and \"double\" */", 9),
            // Real-world: Java Javadoc with HTML tags (tto-engine pattern, 332 mismatches)
            ("t14", "javadoc_html", "Class", "Util.java", 1, 200, "jdoc", "java", "public", "", "/** Perl's split function and <b>s</b> operation inspired. Uses {@link #substitute substitute()} */", 3),
            ("t15", "javadoc_code", "Method", "StreamSearcher.java", 1, 50, "jcod", "java", "public", "", "/**  * performs a function similar to the Unix <code>strings</code> command */", 2),
            ("t16", "javadoc_p_tag", "Method", "GlobFilenameFilter.java", 1, 30, "jpag", "java", "public", "", "/**    * Filters a filename.    * <p>    * @param dir  The directory.    * @return True if match.    */", 1),
            ("t17", "javadoc_link_generic", "Method", "PatternCache.java", 1, 60, "jlnk", "java", "public", "", "/**    * Returns a {@link PatternCache<T>} instance.    * <p>    * Uses {@link #getPattern getPattern()} internally.    */", 4),
            // Real-world: Ruby paths with backslashes (WTax pattern)
            ("t18", "ruby_backslash_path", "Constant", "consts.rb", 1, 5, "rbsp", "ruby", "", "", "Update allows: <anyBasefolderStructureDesired>\\Protax\\LacerteTax\\...", 0),
            ("t19", "ruby_interpolation", "Constant", "consts.rb", 2, 5, "rbin", "ruby", "", "", "lacerte\\#{YEAR_YY}tax\\\\ + NETBRANCH + \\\\Loader\\\\CDROMWIN\\\\", 0),
            // Real-world: VB6 comments (EasyAcct pattern)
            ("t20", "vb6_comment", "Function", "ad911cal.bas", 1, 20, "vb6c", "basic", "", "", "'---PDB 04/02/02 verify if asset complies with sept 11 01 30% rules", 1),
            ("t21", "vb6_include", "Variable", "ad911cal.bas", 3, 3, "vb6i", "basic", "", "", "'$INCLUDE: 'EZDIMCOM.INC'", 0),
            // Real-world: C# XML doc comments (federal pattern)
            ("t22", "csharp_xmldoc", "Method", "TaxCalc.cs", 1, 15, "csxd", "csharp", "public", "TaxCalc", "/// <summary>Calculates <see cref=\"TaxResult\"/> for given <paramref name=\"input\"/></summary>", 2),
            ("t23", "csharp_generic", "Class", "Repository.cs", 1, 100, "csgn", "csharp", "public", "", "/// <typeparam name=\"T\">Must implement <see cref=\"IEntity{T}\"/></typeparam>", 5),
            // SQL injection-style content
            ("t24", "sql_in_doc", "Function", "db.py", 1, 10, "sqli", "python", "", "", "Runs: SELECT * FROM users WHERE name = 'O\\'Brien' AND id > 0; -- drop table", 1),
            // Markdown in docstrings
            ("t25", "markdown_doc", "Function", "lib.rs", 1, 20, "mkdn", "rust", "public", "", "# Header\n\n```rust\nfn main() { println!(\"hello\"); }\n```\n\n- item `<T>`\n- [link](http://example.com?a=1&b=2)", 3),
            // JSON in docstrings
            ("t26", "json_doc", "Function", "api.py", 1, 10, "json", "python", "", "", "Returns {\"key\": \"value\", \"list\": [1, 2, 3], \"nested\": {\"a\": true}}", 1),
            // XML/HTML entities
            ("t27", "entity_doc", "Function", "parser.rs", 1, 10, "enty", "rust", "", "", "Handles &amp; &lt; &gt; &quot; &#39; entities plus raw < > & \" '", 2),
            // Very long docstring (stress test)
            ("t28", "long_doc", "Function", "big.java", 1, 500, "long", "java", "public", "", &long_doc, 99),
            // Null bytes and control characters
            ("t29", "control_chars", "Function", "ctrl.rs", 1, 5, "ctrl", "rust", "", "", "has \x01 \x02 \x03 control chars and \x7f DEL", 1),
            // Windows CRLF
            ("t30", "crlf_doc", "Function", "win.cs", 1, 5, "crlf", "csharp", "", "", "line1\r\nline2\r\nline3", 1),
            // Deeply nested generics (Java/C#)
            ("t31", "nested_generics", "Method", "Deep.java", 1, 10, "deep", "java", "public", "", "Map<String, List<Pair<Integer, Consumer<? super T>>>> process()", 8),
            // Percent and special URL chars
            ("t32", "url_doc", "Function", "http.py", 1, 5, "urls", "python", "", "", "GET /api/v1/users?name=John%20Doe&age=30#section HTTP/1.1", 1),
            // Pipe chars (can confuse some parsers)
            ("t33", "pipe_doc", "Function", "sh.rs", 1, 5, "pipe", "rust", "", "", "cat file.txt | grep 'pattern' | awk '{print $1}' | sort -u", 1),
            // Regex with all special chars
            ("t34", "regex_full", "Function", "re.py", 1, 5, "regx", "python", "", "", "^(?:https?://)?(?:www\\.)?([^/?#]+)(?:[/?#]|$)", 3),
            // Triple quotes and mixed quotes
            ("t35", "triple_quote", "Function", "doc.py", 1, 5, "trpl", "python", "", "", "\"\"\"This is a '''triple quoted''' \"docstring\" with 'mixed' quotes\"\"\"", 1),
        ];

        println!(
            "=== Parquet Quality Test ({} edge cases) ===\n",
            test_rows.len()
        );

        // === Method A: Direct parquet COPY FROM (proposed new path) ===
        let _ = conn.query("DROP TABLE IF EXISTS QualParquet");
        conn.query(&full_schema.replace("%TABLE%", "QualParquet"))?;

        let pq_path = std::env::temp_dir().join("quality_test.parquet");
        {
            let ids: Vec<&str> = test_rows.iter().map(|r| r.0).collect();
            let names: Vec<&str> = test_rows.iter().map(|r| r.1).collect();
            let kinds: Vec<&str> = test_rows.iter().map(|r| r.2).collect();
            let files: Vec<&str> = test_rows.iter().map(|r| r.3).collect();
            let sls: Vec<i64> = test_rows.iter().map(|r| r.4).collect();
            let els: Vec<i64> = test_rows.iter().map(|r| r.5).collect();
            let sigs: Vec<&str> = test_rows.iter().map(|r| r.6).collect();
            let langs: Vec<&str> = test_rows.iter().map(|r| r.7).collect();
            let viss: Vec<&str> = test_rows.iter().map(|r| r.8).collect();
            let pars: Vec<&str> = test_rows.iter().map(|r| r.9).collect();
            let docs: Vec<&str> = test_rows.iter().map(|r| r.10).collect();
            let comps: Vec<i64> = test_rows.iter().map(|r| r.11).collect();

            parquet_loader::write_node_parquet(
                &pq_path,
                &[
                    ("id", DataType::Utf8),
                    ("name", DataType::Utf8),
                    ("kind", DataType::Utf8),
                    ("file", DataType::Utf8),
                    ("start_line", DataType::Int64),
                    ("end_line", DataType::Int64),
                    ("signature_hash", DataType::Utf8),
                    ("language", DataType::Utf8),
                    ("visibility", DataType::Utf8),
                    ("parent", DataType::Utf8),
                    ("docstring", DataType::Utf8),
                    ("complexity", DataType::Int64),
                ],
                vec![
                    Arc::new(StringArray::from(ids)),
                    Arc::new(StringArray::from(names)),
                    Arc::new(StringArray::from(kinds)),
                    Arc::new(StringArray::from(files)),
                    Arc::new(Int64Array::from(sls)),
                    Arc::new(Int64Array::from(els)),
                    Arc::new(StringArray::from(sigs)),
                    Arc::new(StringArray::from(langs)),
                    Arc::new(StringArray::from(viss)),
                    Arc::new(StringArray::from(pars)),
                    Arc::new(StringArray::from(docs)),
                    Arc::new(Int64Array::from(comps)),
                ],
            )?;
        }
        conn.query(&format!("COPY QualParquet (id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity) FROM '{}'", fwd_slash_path(&pq_path)))?;

        // === Method B: DELETE + COPY FROM parquet (proposed incremental path) ===
        let _ = conn.query("DROP TABLE IF EXISTS QualDeleteCopy");
        conn.query(&full_schema.replace("%TABLE%", "QualDeleteCopy"))?;

        // Seed with dummy data first
        conn.query("CREATE (:QualDeleteCopy {id: 'dummy_1', name: 'old', kind: 'X', file: 'old.rs', start_line: 0, end_line: 0, signature_hash: '', language: '', visibility: '', parent: '', docstring: '', complexity: 0})")?;
        conn.query("CREATE (:QualDeleteCopy {id: 'dummy_2', name: 'old2', kind: 'X', file: 'old.rs', start_line: 0, end_line: 0, signature_hash: '', language: '', visibility: '', parent: '', docstring: '', complexity: 0})")?;

        // DELETE old rows then COPY FROM parquet
        conn.query("MATCH (n:QualDeleteCopy) DELETE n")?;
        conn.query(&format!("COPY QualDeleteCopy (id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity) FROM '{}'", fwd_slash_path(&pq_path)))?;

        // === Read back and compare ===
        let fields = [
            "id",
            "name",
            "kind",
            "file",
            "start_line",
            "end_line",
            "signature_hash",
            "language",
            "visibility",
            "parent",
            "docstring",
            "complexity",
        ];
        let field_list = fields
            .iter()
            .map(|f| format!("s.{f}"))
            .collect::<Vec<_>>()
            .join(", ");

        let read_all = |table: &str| -> Result<Vec<Vec<String>>> {
            let r = conn.query(&format!(
                "MATCH (s:{table}) RETURN {field_list} ORDER BY s.id"
            ))?;
            let mut out = Vec::new();
            for row in r {
                out.push(row.iter().map(|v| v.to_string()).collect());
            }
            Ok(out)
        };

        let pq_rows = read_all("QualParquet")?;
        let dc_rows = read_all("QualDeleteCopy")?;

        // Compare Parquet vs DELETE+COPY
        println!("--- Parquet vs DELETE+COPY ---");
        let mut pass = 0;
        let mut fail = 0;
        for (i, (pr, dr)) in pq_rows.iter().zip(dc_rows.iter()).enumerate() {
            for (fi, field) in fields.iter().enumerate() {
                if pr.get(fi) != dr.get(fi) {
                    println!("  MISMATCH row={i} field={field}:");
                    println!("    parquet:      {:?}", pr.get(fi));
                    println!("    delete+copy:  {:?}", dr.get(fi));
                    fail += 1;
                } else {
                    pass += 1;
                }
            }
        }
        println!("  Result: {} passed, {} failed", pass, fail);

        // Compare Parquet vs expected (ground truth = input test data)
        // Use ID-based lookup since ORDER BY sorts lexicographically (t10 < t2)
        println!("\n--- Parquet vs Ground Truth ---");
        let mut gt_pass = 0;
        let mut gt_fail = 0;
        let stored_by_id: HashMap<&str, &Vec<String>> = pq_rows
            .iter()
            .filter_map(|r| r.first().map(|id| (id.as_str(), r)))
            .collect();
        for row in &test_rows {
            let expected = vec![
                row.0.to_string(),
                row.1.to_string(),
                row.2.to_string(),
                row.3.to_string(),
                row.4.to_string(),
                row.5.to_string(),
                row.6.to_string(),
                row.7.to_string(),
                row.8.to_string(),
                row.9.to_string(),
                row.10.to_string(),
                row.11.to_string(),
            ];
            if let Some(stored) = stored_by_id.get(row.0) {
                for (fi, field) in fields.iter().enumerate() {
                    let stored_val = stored.get(fi).map(|s| s.as_str()).unwrap_or("");
                    let expected_val = &expected[fi];
                    if stored_val == expected_val {
                        gt_pass += 1;
                    } else {
                        println!("  MISMATCH id={} field={field}:", row.0);
                        println!("    expected: {:?}", expected_val);
                        println!("    stored:   {:?}", stored_val);
                        gt_fail += 1;
                    }
                }
            } else {
                println!("  MISSING: id={} not found in stored data", row.0);
                gt_fail += 1;
            }
        }
        println!("  Result: {} passed, {} failed", gt_pass, gt_fail);

        if fail == 0 && gt_fail == 0 {
            println!("\n=== ALL TESTS PASSED -- zero quality loss ===");
        } else {
            println!("\n=== QUALITY ISSUES DETECTED ===");
        }

        // Cleanup
        let _ = conn.query("DROP TABLE QualParquet");
        let _ = conn.query("DROP TABLE QualDeleteCopy");
        let _ = std::fs::remove_file(&pq_path);
        Ok(())
    }

    /// Benchmark: compare COPY FROM CSV vs UNWIND for bulk symbol inserts.
    /// Creates isolated test tables, measures both approaches, prints results.
    pub fn benchmark_bulk_write(&self, n: usize) -> Result<()> {
        let conn = self.connection()?;

        // Setup isolated test tables
        let _ = conn.query("DROP TABLE IF EXISTS BenchSymbolCopy");
        let _ = conn.query("DROP TABLE IF EXISTS BenchSymbolUnwind");
        conn.query("CREATE NODE TABLE BenchSymbolCopy(id STRING, name STRING, kind STRING, file STRING, PRIMARY KEY(id))")?;
        conn.query("CREATE NODE TABLE BenchSymbolUnwind(id STRING, name STRING, kind STRING, file STRING, PRIMARY KEY(id))")?;

        // --- COPY FROM CSV ---
        let csv_path = std::env::temp_dir().join("infigraph_bench_symbols.csv");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&csv_path)?;
            writeln!(f, "id,name,kind,file")?;
            for i in 0..n {
                writeln!(f, "copy_{i},func_{i},Function,bench.rs")?;
            }
        }
        let t0 = std::time::Instant::now();
        conn.query(&format!(
            "COPY BenchSymbolCopy FROM '{}' (header=true)",
            fwd_slash_path(&csv_path)
        ))?;
        let copy_ms = t0.elapsed().as_millis();

        // --- UNWIND ---
        const CHUNK: usize = 2000;
        let rows: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    "{{id: 'unwind_{i}', name: 'func_{i}', kind: 'Function', file: 'bench.rs'}}"
                )
            })
            .collect();
        let t1 = std::time::Instant::now();
        for chunk in rows.chunks(CHUNK) {
            conn.query(&format!(
                "UNWIND [{}] AS s CREATE (:BenchSymbolUnwind {{id: s.id, name: s.name, kind: s.kind, file: s.file}})",
                chunk.join(", ")
            ))?;
        }
        let unwind_ms = t1.elapsed().as_millis();

        println!("Bulk write benchmark ({n} symbols):");
        println!("  COPY FROM CSV : {}ms", copy_ms);
        println!("  UNWIND chunks : {}ms", unwind_ms);
        println!(
            "  Speedup       : {:.1}x",
            unwind_ms as f64 / copy_ms.max(1) as f64
        );

        // Cleanup
        let _ = conn.query("DROP TABLE BenchSymbolCopy");
        let _ = conn.query("DROP TABLE BenchSymbolUnwind");
        let _ = std::fs::remove_file(&csv_path);

        Ok(())
    }

    /// Benchmark: CSV vs Parquet vs UNWIND -- apple-to-apple with real symbol data.
    /// Tests performance AND data integrity (docstrings with <, >, quotes, unicode).
    pub fn benchmark_parquet_vs_csv(&self) -> Result<()> {
        let conn = self.connection()?;

        let result = conn.query(
            "MATCH (s:Symbol) RETURN s.id, s.name, s.kind, s.file, s.start_line, s.end_line, s.signature_hash, s.language, s.visibility, s.parent, s.docstring, s.complexity"
        )?;
        let mut rows: Vec<Vec<String>> = Vec::new();
        for row in result {
            rows.push(row.iter().map(|v| v.to_string()).collect());
        }
        let n = rows.len();
        println!("Loaded {} real symbols from graph", n);

        let full_schema = "CREATE NODE TABLE %TABLE%(id STRING, name STRING, kind STRING, file STRING, start_line INT64, end_line INT64, signature_hash STRING, language STRING, visibility STRING, parent STRING, docstring STRING, complexity INT64, PRIMARY KEY(id))";
        let fields_list = "id, name, kind, file, start_line, end_line, signature_hash, language, visibility, parent, docstring, complexity";

        // ===== 1. COPY FROM CSV (TSV) =====
        let _ = conn.query("DROP TABLE IF EXISTS BenchCSV");
        conn.query(&full_schema.replace("%TABLE%", "BenchCSV"))?;

        let csv_path = std::env::temp_dir().join("infigraph_bench_csv.csv");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&csv_path)?;
            writeln!(f, "id\tname\tkind\tfile\tstart_line\tend_line\tsignature_hash\tlanguage\tvisibility\tparent\tdocstring\tcomplexity")?;
            let tsv_field = |s: &str| -> String { s.replace(['\t', '\n', '\r'], " ") };
            for row in &rows {
                writeln!(
                    f,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    tsv_field(&row[0]),
                    tsv_field(&row[1]),
                    tsv_field(&row[2]),
                    tsv_field(&row[3]),
                    row[4],
                    row[5],
                    tsv_field(&row[6]),
                    tsv_field(&row[7]),
                    tsv_field(&row[8]),
                    tsv_field(&row[9]),
                    tsv_field(&row[10]),
                    row[11]
                )?;
            }
        }
        let csv_size = std::fs::metadata(&csv_path).map(|m| m.len()).unwrap_or(0);
        let t0 = std::time::Instant::now();
        conn.query(&format!(
            "COPY BenchCSV FROM '{}' (header=true, delim='\\t')",
            fwd_slash_path(&csv_path)
        ))?;
        let csv_ms = t0.elapsed().as_millis();

        // ===== 2. COPY FROM Parquet =====
        let _ = conn.query("DROP TABLE IF EXISTS BenchParquet");
        conn.query(&full_schema.replace("%TABLE%", "BenchParquet"))?;

        let pq_path = std::env::temp_dir().join("infigraph_bench.parquet");
        {
            let ids: Vec<&str> = rows.iter().map(|r| r[0].as_str()).collect();
            let names: Vec<&str> = rows.iter().map(|r| r[1].as_str()).collect();
            let kinds: Vec<&str> = rows.iter().map(|r| r[2].as_str()).collect();
            let files: Vec<&str> = rows.iter().map(|r| r[3].as_str()).collect();
            let start_lines: Vec<i64> = rows.iter().map(|r| r[4].parse().unwrap_or(0)).collect();
            let end_lines: Vec<i64> = rows.iter().map(|r| r[5].parse().unwrap_or(0)).collect();
            let sig_hashes: Vec<&str> = rows.iter().map(|r| r[6].as_str()).collect();
            let languages: Vec<&str> = rows.iter().map(|r| r[7].as_str()).collect();
            let visibilities: Vec<&str> = rows.iter().map(|r| r[8].as_str()).collect();
            let parents: Vec<&str> = rows.iter().map(|r| r[9].as_str()).collect();
            let docstrings: Vec<&str> = rows.iter().map(|r| r[10].as_str()).collect();
            let complexities: Vec<i64> = rows.iter().map(|r| r[11].parse().unwrap_or(0)).collect();

            parquet_loader::write_node_parquet(
                &pq_path,
                &[
                    ("id", DataType::Utf8),
                    ("name", DataType::Utf8),
                    ("kind", DataType::Utf8),
                    ("file", DataType::Utf8),
                    ("start_line", DataType::Int64),
                    ("end_line", DataType::Int64),
                    ("signature_hash", DataType::Utf8),
                    ("language", DataType::Utf8),
                    ("visibility", DataType::Utf8),
                    ("parent", DataType::Utf8),
                    ("docstring", DataType::Utf8),
                    ("complexity", DataType::Int64),
                ],
                vec![
                    Arc::new(StringArray::from(ids)),
                    Arc::new(StringArray::from(names)),
                    Arc::new(StringArray::from(kinds)),
                    Arc::new(StringArray::from(files)),
                    Arc::new(Int64Array::from(start_lines)),
                    Arc::new(Int64Array::from(end_lines)),
                    Arc::new(StringArray::from(sig_hashes)),
                    Arc::new(StringArray::from(languages)),
                    Arc::new(StringArray::from(visibilities)),
                    Arc::new(StringArray::from(parents)),
                    Arc::new(StringArray::from(docstrings)),
                    Arc::new(Int64Array::from(complexities)),
                ],
            )?;
        }
        let pq_size = std::fs::metadata(&pq_path).map(|m| m.len()).unwrap_or(0);
        let t1 = std::time::Instant::now();
        conn.query(&format!(
            "COPY BenchParquet ({fields_list}) FROM '{}'",
            fwd_slash_path(&pq_path)
        ))?;
        let pq_ms = t1.elapsed().as_millis();

        // ===== 3. UNWIND =====
        let _ = conn.query("DROP TABLE IF EXISTS BenchUnwind");
        conn.query(&full_schema.replace("%TABLE%", "BenchUnwind"))?;

        const CHUNK: usize = 2000;
        let unwind_rows: Vec<String> = rows.iter().map(|row| {
            format!("{{id: '{}', name: '{}', kind: '{}', file: '{}', start_line: {}, end_line: {}, signature_hash: '{}', language: '{}', visibility: '{}', parent: '{}', docstring: '{}', complexity: {}}}",
                escape(&row[0]), escape(&row[1]), escape(&row[2]), escape(&row[3]),
                row[4], row[5],
                escape(&row[6]), escape(&row[7]), escape(&row[8]),
                escape(&row[9]), escape(&row[10]), row[11])
        }).collect();
        let t2 = std::time::Instant::now();
        for chunk in unwind_rows.chunks(CHUNK) {
            conn.query(&format!(
                "UNWIND [{}] AS s CREATE (:BenchUnwind {{id: s.id, name: s.name, kind: s.kind, file: s.file, start_line: s.start_line, end_line: s.end_line, signature_hash: s.signature_hash, language: s.language, visibility: s.visibility, parent: s.parent, docstring: s.docstring, complexity: s.complexity}})",
                chunk.join(", ")
            ))?;
        }
        let unwind_ms = t2.elapsed().as_millis();

        // ===== Results =====
        println!("\n=== Bulk Write Benchmark ({n} symbols) ===\n");
        println!(
            "  {:20} {:>8} {:>12} {:>10}",
            "Method", "Time", "Throughput", "File Size"
        );
        println!(
            "  {:20} {:>8} {:>12} {:>10}",
            "------", "----", "----------", "---------"
        );
        println!(
            "  {:20} {:>7}ms {:>9.0}/sec {:>9}KB",
            "COPY FROM CSV (TSV)",
            csv_ms,
            n as f64 / csv_ms.max(1) as f64 * 1000.0,
            csv_size / 1024
        );
        println!(
            "  {:20} {:>7}ms {:>9.0}/sec {:>9}KB",
            "COPY FROM Parquet",
            pq_ms,
            n as f64 / pq_ms.max(1) as f64 * 1000.0,
            pq_size / 1024
        );
        println!(
            "  {:20} {:>7}ms {:>9.0}/sec {:>10}",
            "UNWIND chunks",
            unwind_ms,
            n as f64 / unwind_ms.max(1) as f64 * 1000.0,
            "N/A"
        );
        println!(
            "\n  CSV vs Parquet     : {:.2}x",
            csv_ms as f64 / pq_ms.max(1) as f64
        );
        println!(
            "  Parquet vs UNWIND  : {:.1}x",
            unwind_ms as f64 / pq_ms.max(1) as f64
        );

        // ===== Data Integrity =====
        println!("\n=== Data Integrity Check ===\n");
        let fields = [
            "id",
            "name",
            "kind",
            "file",
            "start_line",
            "end_line",
            "signature_hash",
            "language",
            "visibility",
            "parent",
            "docstring",
            "complexity",
        ];
        let field_list = fields
            .iter()
            .map(|f| format!("s.{f}"))
            .collect::<Vec<_>>()
            .join(", ");

        let read_all = |table: &str| -> Result<Vec<Vec<String>>> {
            let r = conn.query(&format!(
                "MATCH (s:{table}) RETURN {field_list} ORDER BY s.id"
            ))?;
            let mut out = Vec::new();
            for row in r {
                out.push(row.iter().map(|v| v.to_string()).collect());
            }
            Ok(out)
        };

        let csv_rows = read_all("BenchCSV")?;
        let pq_rows = read_all("BenchParquet")?;
        let uw_rows = read_all("BenchUnwind")?;

        let compare = |name: &str, a: &[Vec<String>], b: &[Vec<String>]| {
            let mut mismatches = 0usize;
            if a.len() != b.len() {
                println!("  {name}: ROW COUNT MISMATCH ({} vs {})", a.len(), b.len());
                return;
            }
            for (i, (ar, br)) in a.iter().zip(b.iter()).enumerate() {
                for (fi, field) in fields.iter().enumerate() {
                    if ar.get(fi) != br.get(fi) {
                        if mismatches < 5 {
                            println!("  {name} MISMATCH row={i} field={field}:");
                            println!("    left:  {:?}", ar.get(fi));
                            println!("    right: {:?}", br.get(fi));
                        }
                        mismatches += 1;
                    }
                }
            }
            if mismatches == 0 {
                println!(
                    "  {name}: PASS -- all {n} symbols x {} fields match",
                    fields.len()
                );
            } else {
                println!("  {name}: FAIL -- {mismatches} mismatches");
            }
        };

        compare("CSV vs Parquet", &csv_rows, &pq_rows);
        compare("CSV vs UNWIND", &csv_rows, &uw_rows);
        compare("Parquet vs UNWIND", &pq_rows, &uw_rows);

        // Cleanup
        let _ = conn.query("DROP TABLE BenchCSV");
        let _ = conn.query("DROP TABLE BenchParquet");
        let _ = conn.query("DROP TABLE BenchUnwind");
        let _ = std::fs::remove_file(&csv_path);
        let _ = std::fs::remove_file(&pq_path);

        Ok(())
    }
}
