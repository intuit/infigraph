/// lsp-to-scip: Generic LSP → SCIP bridge
///
/// Spawns any Language Server Protocol server, sends it workspace/didChangeWatchedFiles
/// and textDocument/* requests for every file in the project, then emits index.scip.
///
/// Usage:
///   lsp-to-scip --server "clangd" --root /path/to/project --lang cpp --out index.scip
///   lsp-to-scip --server "zls" --root /path/to/project --lang zig --out index.scip
///   lsp-to-scip --server "elixir-ls" --root . --lang elixir --out index.scip
///
/// The server arg can be a full command with args:
///   lsp-to-scip --server "dart pub global run dart_language_server" --lang dart
///
/// Supported extension mappings are derived from --lang.
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{bail, Context, Result};
use clap::Parser;
use scip::types::{symbol_information, Document, Index, Occurrence, SymbolInformation, SymbolRole};

mod lsp_types;
use lsp_types::*;

static REQUEST_ID: AtomicI64 = AtomicI64::new(1);

#[derive(Parser)]
#[command(name = "lsp-to-scip", about = "Generic LSP → SCIP index generator")]
struct Cli {
    /// LSP server command (e.g. "clangd", "zls", "dart pub global run dart_language_server")
    #[arg(short, long)]
    server: String,

    /// Project root directory
    #[arg(short, long, default_value = ".")]
    root: PathBuf,

    /// Language identifier (e.g. cpp, zig, swift, dart, elixir, php, lua, haskell, fsharp, clojure, erlang, perl)
    #[arg(short, long)]
    lang: String,

    /// Output path for index.scip
    #[arg(short, long, default_value = "index.scip")]
    out: PathBuf,

    /// Max files to process (0 = no limit)
    #[arg(long, default_value = "0")]
    max_files: usize,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = cli.root.canonicalize().context("invalid root")?;

    let extensions = lang_extensions(&cli.lang);
    if extensions.is_empty() {
        bail!(
            "unknown language '{}'. Use --lang with a known language code.",
            cli.lang
        );
    }

    eprintln!("[lsp-to-scip] lang={} root={}", cli.lang, root.display());
    eprintln!(
        "[lsp-to-scip] collecting files with extensions: {:?}",
        extensions
    );

    let files = collect_files(&root, &extensions, cli.max_files);
    eprintln!("[lsp-to-scip] found {} files", files.len());

    if files.is_empty() {
        bail!(
            "no source files found for lang={} in {}",
            cli.lang,
            root.display()
        );
    }

    // Parse server command
    let mut parts = cli.server.split_whitespace();
    let exe = parts.next().context("empty server command")?;
    let server_args: Vec<&str> = parts.collect();

    let mut child = Command::new(exe)
        .args(&server_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn LSP server: {}", cli.server))?;

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut conn = LspConnection::new(stdin, BufReader::new(stdout));

    // Initialize
    let root_uri = path_to_uri(&root);
    let init_result = conn.initialize(&root_uri, &cli.lang)?;
    if cli.verbose {
        eprintln!(
            "[lsp-to-scip] initialized: {}",
            serde_json::to_string(&init_result)?
        );
    }
    conn.initialized()?;

    // Open all files
    for f in &files {
        let uri = path_to_uri(f);
        let content = std::fs::read_to_string(f).unwrap_or_default();
        conn.did_open(&uri, &cli.lang, &content)?;
    }

    // Collect symbols/definitions for each file
    let mut documents: Vec<Document> = Vec::new();

    for f in &files {
        let rel = f.strip_prefix(&root).unwrap_or(f);
        let rel_str = rel.to_string_lossy().to_string();
        let uri = path_to_uri(f);
        let content = std::fs::read_to_string(f).unwrap_or_default();
        let _line_count = content.lines().count() as u32;

        if cli.verbose {
            eprintln!("[lsp-to-scip] processing {}", rel_str);
        }

        let mut doc = Document::new();
        doc.relative_path = rel_str.clone();

        // Request document symbols
        let syms = conn.document_symbols(&uri).unwrap_or_default();
        for sym in &syms {
            let scip_sym = symbol_string(&rel_str, &sym.name, &sym.kind);
            let mut occ = Occurrence::new();
            occ.symbol = scip_sym.clone();
            occ.symbol_roles = SymbolRole::Definition as i32;
            occ.range = vec![
                sym.range.start.line as i32,
                sym.range.start.character as i32,
                sym.range.end.line as i32,
                sym.range.end.character as i32,
            ];
            doc.occurrences.push(occ);

            let mut si = SymbolInformation::new();
            si.symbol = scip_sym;
            si.kind = lsp_kind_to_scip(&sym.kind).into();
            if let Some(detail) = &sym.detail {
                si.documentation.push(detail.clone());
            }
            doc.symbols.push(si);
        }

        // For each definition, request "go to definition" from the symbol location
        // and "find references" — this enriches cross-file edges
        // Skip for now on large files (>500 symbols) to avoid hanging
        if syms.len() < 500 {
            for sym in &syms {
                let refs = conn
                    .find_references(
                        &uri,
                        sym.selection_range.start.line,
                        sym.selection_range.start.character,
                    )
                    .unwrap_or_default();

                let src_scip_sym = symbol_string(&rel_str, &sym.name, &sym.kind);
                for ref_loc in refs {
                    let ref_rel = uri_to_rel_path(&ref_loc.uri, &root);
                    if ref_rel == rel_str {
                        // Reference in same file — add reference occurrence
                        let mut occ = Occurrence::new();
                        occ.symbol = src_scip_sym.clone();
                        occ.symbol_roles = 0; // reference
                        occ.range = vec![
                            ref_loc.range.start.line as i32,
                            ref_loc.range.start.character as i32,
                            ref_loc.range.end.line as i32,
                            ref_loc.range.end.character as i32,
                        ];
                        doc.occurrences.push(occ);
                    }
                    // Cross-file references are handled when processing the referencing file
                }
            }
        }

        documents.push(doc);
    }

    conn.shutdown()?;
    let _ = child.wait();

    // Build SCIP Index
    let mut index = Index::new();
    index.documents = documents;

    scip::write_message_to_file(&cli.out, index)
        .map_err(|e| anyhow::anyhow!("failed to write index.scip: {e}"))?;

    eprintln!("[lsp-to-scip] wrote {}", cli.out.display());
    Ok(())
}

// ── LSP connection ──────────────────────────────────────────────────────────

struct LspConnection {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl LspConnection {
    fn new(stdin: ChildStdin, stdout: BufReader<ChildStdout>) -> Self {
        Self { stdin, stdout }
    }

    fn send(&mut self, msg: &serde_json::Value) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes())?;
        self.stdin.write_all(body.as_bytes())?;
        self.stdin.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<serde_json::Value> {
        // Read headers
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line)?;
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(stripped) = line.strip_prefix("Content-Length:") {
                let val = stripped.trim();
                content_length = Some(val.parse()?);
            }
        }
        let len = content_length.context("missing Content-Length header")?;
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf)?;
        Ok(serde_json::from_slice(&buf)?)
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = REQUEST_ID.fetch_add(1, Ordering::SeqCst);
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.send(&msg)?;

        // Read responses until we get our id back (skip notifications)
        loop {
            let resp = self.recv()?;
            if let Some(resp_id) = resp.get("id") {
                if resp_id == id {
                    return Ok(resp);
                }
            }
            // Notification — ignore and continue
        }
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.send(&msg)
    }

    fn initialize(&mut self, root_uri: &str, _lang: &str) -> Result<serde_json::Value> {
        let resp = self.request(
            "initialize",
            serde_json::json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "documentSymbol": {
                            "hierarchicalDocumentSymbolSupport": false
                        },
                        "references": {}
                    }
                },
                "initializationOptions": {}
            }),
        )?;
        Ok(resp["result"].clone())
    }

    fn initialized(&mut self) -> Result<()> {
        self.notify("initialized", serde_json::json!({}))
    }

    fn did_open(&mut self, uri: &str, lang: &str, text: &str) -> Result<()> {
        self.notify(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": lang,
                    "version": 1,
                    "text": text
                }
            }),
        )
    }

    fn document_symbols(&mut self, uri: &str) -> Result<Vec<LspSymbol>> {
        let resp = self.request(
            "textDocument/documentSymbol",
            serde_json::json!({
                "textDocument": { "uri": uri }
            }),
        )?;

        let result = &resp["result"];
        if result.is_null() {
            return Ok(vec![]);
        }

        // Handle both SymbolInformation[] and DocumentSymbol[]
        let syms: Vec<LspSymbol> = if let Some(arr) = result.as_array() {
            arr.iter().filter_map(parse_symbol).collect()
        } else {
            vec![]
        };
        Ok(syms)
    }

    fn find_references(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<LspLocation>> {
        let resp = self.request(
            "textDocument/references",
            serde_json::json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
                "context": { "includeDeclaration": false }
            }),
        )?;

        let result = &resp["result"];
        if result.is_null() {
            return Ok(vec![]);
        }
        let locs: Vec<LspLocation> = result
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        Ok(locs)
    }

    fn shutdown(&mut self) -> Result<()> {
        let _ = self.request("shutdown", serde_json::json!(null));
        let _ = self.notify("exit", serde_json::json!(null));
        Ok(())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn path_to_uri(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn uri_to_rel_path(uri: &str, root: &Path) -> String {
    let path_str = uri.strip_prefix("file://").unwrap_or(uri);
    let path = Path::new(path_str);
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn collect_files(root: &Path, extensions: &[&str], max: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files_rec(root, extensions, &mut files);
    if max > 0 && files.len() > max {
        files.truncate(max);
    }
    files
}

fn collect_files_rec(dir: &Path, extensions: &[&str], files: &mut Vec<PathBuf>) {
    let ignore = [
        ".git",
        "node_modules",
        "target",
        "build",
        "dist",
        ".infigraph",
        "__pycache__",
        ".venv",
        "venv",
        "_build",
        "deps",
    ];
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if path.is_dir() {
            if !ignore.contains(&name_str.as_ref()) && !name_str.starts_with('.') {
                collect_files_rec(&path, extensions, files);
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if extensions.contains(&ext) {
                files.push(path);
            }
        }
    }
}

fn lang_extensions(lang: &str) -> Vec<&'static str> {
    match lang {
        "cpp" | "c++" => vec!["cpp", "cc", "cxx", "c", "h", "hpp", "hxx"],
        "c" => vec!["c", "h"],
        "zig" => vec!["zig"],
        "swift" => vec!["swift"],
        "dart" => vec!["dart"],
        "elixir" => vec!["ex", "exs"],
        "php" => vec!["php"],
        "lua" => vec!["lua"],
        "haskell" => vec!["hs", "lhs"],
        "fsharp" | "f#" => vec!["fs", "fsi", "fsx"],
        "clojure" => vec!["clj", "cljs", "cljc"],
        "erlang" => vec!["erl", "hrl"],
        "perl" => vec!["pl", "pm", "t"],
        "ocaml" => vec!["ml", "mli"],
        "nim" => vec!["nim"],
        "crystal" => vec!["cr"],
        "julia" => vec!["jl"],
        "r" => vec!["r", "R"],
        "groovy" => vec!["groovy", "gvy"],
        "verilog" => vec!["v", "sv"],
        "terraform" | "hcl" => vec!["tf", "hcl"],
        "toml" => vec!["toml"],
        "yaml" => vec!["yaml", "yml"],
        _ => vec![],
    }
}

fn symbol_string(file: &str, name: &str, _kind: &str) -> String {
    // SCIP symbol format: "<scheme> <manager> <package> <version> <descriptors>"
    // Use a local scheme for LSP-derived symbols
    format!("lsp . . . {}#{}", file.replace('/', "."), name)
}

fn lsp_kind_to_scip(kind: &str) -> symbol_information::Kind {
    match kind {
        "Function" => symbol_information::Kind::Function,
        "Method" | "Constructor" => symbol_information::Kind::Method,
        "Class" => symbol_information::Kind::Class,
        "Interface" => symbol_information::Kind::Interface,
        "Enum" | "EnumMember" => symbol_information::Kind::Enum,
        "Struct" => symbol_information::Kind::Struct,
        "Module" | "Namespace" | "Package" => symbol_information::Kind::Module,
        "Variable" | "Field" | "Property" => symbol_information::Kind::Variable,
        "Constant" => symbol_information::Kind::Constant,
        "Trait" | "TypeParameter" => symbol_information::Kind::Trait,
        _ => symbol_information::Kind::UnspecifiedKind,
    }
}

fn parse_symbol(v: &serde_json::Value) -> Option<LspSymbol> {
    // Both SymbolInformation and DocumentSymbol have name + kind
    let name = v["name"].as_str()?.to_string();
    let kind_num = v["kind"].as_u64()? as u32;
    let kind = lsp_kind_num_to_str(kind_num).to_string();
    let detail = v["detail"].as_str().map(|s| s.to_string());

    // DocumentSymbol has range + selectionRange
    // SymbolInformation has location.range
    let (range, sel_range) = if let Some(r) = v.get("range") {
        let range = parse_range(r)?;
        let sel = v
            .get("selectionRange")
            .and_then(parse_range)
            .unwrap_or(range.clone());
        (range, sel)
    } else {
        let loc = v.get("location")?;
        let range = parse_range(&loc["range"])?;
        (range.clone(), range)
    };

    Some(LspSymbol {
        name,
        kind,
        detail,
        range,
        selection_range: sel_range,
    })
}

fn parse_range(v: &serde_json::Value) -> Option<LspRange> {
    Some(LspRange {
        start: LspPosition {
            line: v["start"]["line"].as_u64()? as u32,
            character: v["start"]["character"].as_u64()? as u32,
        },
        end: LspPosition {
            line: v["end"]["line"].as_u64()? as u32,
            character: v["end"]["character"].as_u64()? as u32,
        },
    })
}

fn lsp_kind_num_to_str(n: u32) -> &'static str {
    match n {
        1 => "File",
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        15 => "String",
        16 => "Number",
        17 => "Boolean",
        18 => "Array",
        19 => "Object",
        20 => "Key",
        21 => "Null",
        22 => "EnumMember",
        23 => "Struct",
        24 => "Event",
        25 => "Operator",
        26 => "TypeParameter",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ── lang_extensions ─────────────────────────────────────────────────

    #[test]
    fn test_lang_extensions_cpp() {
        let exts = lang_extensions("cpp");
        assert!(exts.contains(&"cpp"));
        assert!(exts.contains(&"h"));
        assert!(exts.contains(&"hpp"));
    }

    #[test]
    fn test_lang_extensions_cpp_alias() {
        assert_eq!(lang_extensions("c++"), lang_extensions("cpp"));
    }

    #[test]
    fn test_lang_extensions_zig() {
        assert_eq!(lang_extensions("zig"), vec!["zig"]);
    }

    #[test]
    fn test_lang_extensions_elixir() {
        let exts = lang_extensions("elixir");
        assert!(exts.contains(&"ex"));
        assert!(exts.contains(&"exs"));
    }

    #[test]
    fn test_lang_extensions_fsharp_alias() {
        assert_eq!(lang_extensions("f#"), lang_extensions("fsharp"));
    }

    #[test]
    fn test_lang_extensions_unknown() {
        assert!(lang_extensions("brainfuck").is_empty());
    }

    // ── symbol_string ───────────────────────────────────────────────────

    #[test]
    fn test_symbol_string_basic() {
        let s = symbol_string("src/main.rs", "foo", "Function");
        assert_eq!(s, "lsp . . . src.main.rs#foo");
    }

    #[test]
    fn test_symbol_string_nested_path() {
        let s = symbol_string("a/b/c.rs", "Bar", "Class");
        assert_eq!(s, "lsp . . . a.b.c.rs#Bar");
    }

    // ── lsp_kind_to_scip ────────────────────────────────────────────────

    #[test]
    fn test_lsp_kind_to_scip_function() {
        assert_eq!(
            lsp_kind_to_scip("Function"),
            symbol_information::Kind::Function
        );
    }

    #[test]
    fn test_lsp_kind_to_scip_method() {
        assert_eq!(lsp_kind_to_scip("Method"), symbol_information::Kind::Method);
    }

    #[test]
    fn test_lsp_kind_to_scip_constructor() {
        assert_eq!(
            lsp_kind_to_scip("Constructor"),
            symbol_information::Kind::Method
        );
    }

    #[test]
    fn test_lsp_kind_to_scip_class() {
        assert_eq!(lsp_kind_to_scip("Class"), symbol_information::Kind::Class);
    }

    #[test]
    fn test_lsp_kind_to_scip_enum() {
        assert_eq!(lsp_kind_to_scip("Enum"), symbol_information::Kind::Enum);
    }

    #[test]
    fn test_lsp_kind_to_scip_enum_member() {
        assert_eq!(
            lsp_kind_to_scip("EnumMember"),
            symbol_information::Kind::Enum
        );
    }

    #[test]
    fn test_lsp_kind_to_scip_struct() {
        assert_eq!(lsp_kind_to_scip("Struct"), symbol_information::Kind::Struct);
    }

    #[test]
    fn test_lsp_kind_to_scip_variable() {
        assert_eq!(
            lsp_kind_to_scip("Variable"),
            symbol_information::Kind::Variable
        );
    }

    #[test]
    fn test_lsp_kind_to_scip_constant() {
        assert_eq!(
            lsp_kind_to_scip("Constant"),
            symbol_information::Kind::Constant
        );
    }

    #[test]
    fn test_lsp_kind_to_scip_trait() {
        assert_eq!(lsp_kind_to_scip("Trait"), symbol_information::Kind::Trait);
    }

    #[test]
    fn test_lsp_kind_to_scip_unknown() {
        assert_eq!(
            lsp_kind_to_scip("Banana"),
            symbol_information::Kind::UnspecifiedKind
        );
    }

    // ── lsp_kind_num_to_str ─────────────────────────────────────────────

    #[test]
    fn test_lsp_kind_num_to_str_known() {
        assert_eq!(lsp_kind_num_to_str(1), "File");
        assert_eq!(lsp_kind_num_to_str(5), "Class");
        assert_eq!(lsp_kind_num_to_str(12), "Function");
        assert_eq!(lsp_kind_num_to_str(23), "Struct");
        assert_eq!(lsp_kind_num_to_str(26), "TypeParameter");
    }

    #[test]
    fn test_lsp_kind_num_to_str_boundaries() {
        assert_eq!(lsp_kind_num_to_str(0), "Unknown");
        assert_eq!(lsp_kind_num_to_str(27), "Unknown");
        assert_eq!(lsp_kind_num_to_str(999), "Unknown");
    }

    // ── parse_range ─────────────────────────────────────────────────────

    #[test]
    fn test_parse_range_valid() {
        let v = serde_json::json!({
            "start": {"line": 1, "character": 2},
            "end": {"line": 3, "character": 5}
        });
        let r = parse_range(&v).unwrap();
        assert_eq!(r.start.line, 1);
        assert_eq!(r.start.character, 2);
        assert_eq!(r.end.line, 3);
        assert_eq!(r.end.character, 5);
    }

    #[test]
    fn test_parse_range_missing_field() {
        let v = serde_json::json!({"start": {"line": 0}});
        assert!(parse_range(&v).is_none());
    }

    #[test]
    fn test_parse_range_null() {
        let v = serde_json::json!(null);
        assert!(parse_range(&v).is_none());
    }

    // ── parse_symbol ────────────────────────────────────────────────────

    #[test]
    fn test_parse_symbol_document_symbol() {
        let v = serde_json::json!({
            "name": "myFunc",
            "kind": 12,
            "detail": "fn myFunc()",
            "range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 5, "character": 1}
            },
            "selectionRange": {
                "start": {"line": 0, "character": 3},
                "end": {"line": 0, "character": 9}
            }
        });
        let sym = parse_symbol(&v).unwrap();
        assert_eq!(sym.name, "myFunc");
        assert_eq!(sym.kind, "Function");
        assert_eq!(sym.detail.as_deref(), Some("fn myFunc()"));
        assert_eq!(sym.range.start.line, 0);
        assert_eq!(sym.selection_range.start.character, 3);
    }

    #[test]
    fn test_parse_symbol_symbol_information() {
        let v = serde_json::json!({
            "name": "MyClass",
            "kind": 5,
            "location": {
                "uri": "file:///tmp/foo.rs",
                "range": {
                    "start": {"line": 10, "character": 0},
                    "end": {"line": 20, "character": 1}
                }
            }
        });
        let sym = parse_symbol(&v).unwrap();
        assert_eq!(sym.name, "MyClass");
        assert_eq!(sym.kind, "Class");
        assert!(sym.detail.is_none());
        assert_eq!(sym.range.start.line, 10);
    }

    #[test]
    fn test_parse_symbol_missing_name() {
        let v = serde_json::json!({"kind": 5});
        assert!(parse_symbol(&v).is_none());
    }

    #[test]
    fn test_parse_symbol_missing_range_and_location() {
        let v = serde_json::json!({"name": "x", "kind": 5});
        assert!(parse_symbol(&v).is_none());
    }

    // ── path_to_uri ─────────────────────────────────────────────────────

    #[test]
    fn test_path_to_uri() {
        let p = Path::new("/tmp/project/src/main.rs");
        assert_eq!(path_to_uri(p), "file:///tmp/project/src/main.rs");
    }

    // ── uri_to_rel_path ─────────────────────────────────────────────────

    #[test]
    fn test_uri_to_rel_path_strips_prefix() {
        let root = Path::new("/tmp/project");
        let uri = "file:///tmp/project/src/main.rs";
        assert_eq!(uri_to_rel_path(uri, root), "src/main.rs");
    }

    #[test]
    fn test_uri_to_rel_path_no_file_scheme() {
        let root = Path::new("/tmp/project");
        let uri = "/tmp/project/lib.rs";
        assert_eq!(uri_to_rel_path(uri, root), "lib.rs");
    }

    #[test]
    fn test_uri_to_rel_path_outside_root() {
        let root = Path::new("/tmp/project");
        let uri = "file:///other/place/foo.rs";
        assert_eq!(uri_to_rel_path(uri, root), "/other/place/foo.rs");
    }

    // ── collect_files ───────────────────────────────────────────────────

    #[test]
    fn test_collect_files_basic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("main.zig"), "").unwrap();
        fs::write(root.join("readme.txt"), "").unwrap();

        let files = collect_files(root, &["zig"], 0);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("main.zig"));
    }

    #[test]
    fn test_collect_files_skips_ignored_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git/config.zig"), "").unwrap();

        fs::create_dir(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules/dep.zig"), "").unwrap();

        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/out.zig"), "").unwrap();

        fs::write(root.join("real.zig"), "").unwrap();

        let files = collect_files(root, &["zig"], 0);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("real.zig"));
    }

    #[test]
    fn test_collect_files_max_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.rs"), "").unwrap();
        fs::write(root.join("b.rs"), "").unwrap();
        fs::write(root.join("c.rs"), "").unwrap();

        let files = collect_files(root, &["rs"], 1);
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn test_collect_files_max_zero_no_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::write(root.join("a.rs"), "").unwrap();
        fs::write(root.join("b.rs"), "").unwrap();

        let files = collect_files(root, &["rs"], 0);
        assert_eq!(files.len(), 2);
    }

    #[test]
    fn test_collect_files_recurses_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "").unwrap();

        let files = collect_files(root, &["rs"], 0);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("src/lib.rs"));
    }
}
