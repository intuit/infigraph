use serde::{Deserialize, Serialize};

/// The kind of symbol extracted from source code.
/// Language-agnostic — Python classes, Rust structs, Java interfaces all map here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Interface,
    Trait,
    Enum,
    Module,
    Variable,
    Constant,
    Test,
    Section,
    Route,
    Field,
}

impl SymbolKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "Function",
            Self::Method => "Method",
            Self::Class => "Class",
            Self::Struct => "Struct",
            Self::Interface => "Interface",
            Self::Trait => "Trait",
            Self::Enum => "Enum",
            Self::Module => "Module",
            Self::Variable => "Variable",
            Self::Constant => "Constant",
            Self::Test => "Test",
            Self::Section => "Section",
            Self::Route => "Route",
            Self::Field => "Field",
        }
    }
}

/// A location span in a source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub file: String,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// A symbol (entity) extracted from the AST.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    /// Unique ID: "file::name" or "file::class::method"
    pub id: String,
    pub name: String,
    pub kind: SymbolKind,
    pub span: Span,
    /// Hash of the AST subtree — used for incremental analysis
    pub signature_hash: String,
    /// Parent symbol ID (e.g., method's class)
    pub parent: Option<String>,
    /// Language this was extracted from
    pub language: String,
    /// Visibility: public, private, etc.
    pub visibility: Option<String>,
    /// Docstring extracted from the AST
    pub docstring: Option<String>,
    /// Cyclomatic complexity (1 = no branches; only set for Function/Method/Test)
    pub complexity: u32,
    /// Function/method parameter list (raw text from AST)
    pub parameters: Option<String>,
    /// Return type annotation (raw text from AST)
    pub return_type: Option<String>,
    /// SCIP moniker string this symbol was correlated to during enrichment
    /// (full descriptor path, including disambiguator). `None` until SCIP
    /// enrichment runs and finds a match.
    pub scip_id: Option<String>,
}

/// The kind of relationship between two symbols.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelationKind {
    Calls,
    CalledBy,
    Imports,
    ImportedBy,
    Contains,
    ContainedBy,
    Inherits,
    InheritedBy,
    Implements,
    ImplementedBy,
    Reads,
    Writes,
    TestedBy,
    Tests,
    Custom(String),
}

impl RelationKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Calls => "CALLS",
            Self::CalledBy => "CALLED_BY",
            Self::Imports => "IMPORTS",
            Self::ImportedBy => "IMPORTED_BY",
            Self::Contains => "CONTAINS",
            Self::ContainedBy => "CONTAINED_BY",
            Self::Inherits => "INHERITS",
            Self::InheritedBy => "INHERITED_BY",
            Self::Implements => "IMPLEMENTS",
            Self::ImplementedBy => "IMPLEMENTED_BY",
            Self::Reads => "READS",
            Self::Writes => "WRITES",
            Self::TestedBy => "TESTED_BY",
            Self::Tests => "TESTS",
            Self::Custom(name) => name.as_str(),
        }
    }
}

/// A relationship (edge) between two symbols.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub source_id: String,
    pub target_id: String,
    pub kind: RelationKind,
    pub span: Option<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
}

/// Kind of cross-language bridge detected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BridgeKind {
    /// Rust `extern "C"` / C `extern` FFI
    Ffi,
    /// Java Native Interface
    Jni,
    /// Go cgo (`import "C"`)
    Cgo,
    /// gRPC service definition or generated stub call
    Grpc,
    /// .NET P/Invoke (`DllImport`)
    PInvoke,
    /// Python `ctypes` / `cffi` foreign call
    Ctypes,
    /// WASM import/export boundary
    Wasm,
    /// COM interop / CLR bridge (VB6, VBA)
    Com,
    /// Generic foreign call pattern
    Other(String),
}

impl BridgeKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Ffi => "FFI",
            Self::Jni => "JNI",
            Self::Cgo => "CGO",
            Self::Grpc => "GRPC",
            Self::PInvoke => "P_INVOKE",
            Self::Ctypes => "CTYPES",
            Self::Wasm => "WASM",
            Self::Com => "COM",
            Self::Other(s) => s,
        }
    }
}

/// A detected cross-language bridge point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bridge {
    pub file: String,
    pub line: u32,
    pub kind: BridgeKind,
    /// Name of the foreign symbol/function being bridged to
    pub foreign_symbol: String,
    /// Source language
    pub source_language: String,
    /// Target language (if determinable)
    pub target_language: Option<String>,
    /// Additional context (e.g., library name, proto service)
    pub detail: String,
}

/// The kind of control-flow statement extracted from a function body.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StatementKind {
    If,
    ElseIf,
    Else,
    For,
    While,
    DoWhile,
    Loop,
    Match,
    Case,
    Try,
    Catch,
    Ternary,
    Guard,
}

impl StatementKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::If => "If",
            Self::ElseIf => "ElseIf",
            Self::Else => "Else",
            Self::For => "For",
            Self::While => "While",
            Self::DoWhile => "DoWhile",
            Self::Loop => "Loop",
            Self::Match => "Match",
            Self::Case => "Case",
            Self::Try => "Try",
            Self::Catch => "Catch",
            Self::Ternary => "Ternary",
            Self::Guard => "Guard",
        }
    }
}

/// A control-flow statement inside a function/method body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Statement {
    pub id: String,
    pub kind: StatementKind,
    pub condition: String,
    pub start_line: u32,
    pub end_line: u32,
    pub depth: u32,
    pub parent_symbol: String,
}

/// Result of extracting a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileExtraction {
    pub file: String,
    pub language: String,
    pub content_hash: String,
    pub symbols: Vec<Symbol>,
    pub relations: Vec<Relation>,
    pub statements: Vec<Statement>,
}
