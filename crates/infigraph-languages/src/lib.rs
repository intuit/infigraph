use anyhow::Result;
use infigraph_core::lang::{CustomEdgeDef, LanguagePack, LanguageRegistry};

const PYTHON_ENTITIES: &str = include_str!("../languages/python/entities.scm");
const PYTHON_RELATIONS: &str = include_str!("../languages/python/relations.scm");
const PYTHON_INHERIT_DECOMPOSE: &str = include_str!("../languages/python/inherit_decompose.scm");

const RUST_ENTITIES: &str = include_str!("../languages/rust/entities.scm");
const RUST_RELATIONS: &str = include_str!("../languages/rust/relations.scm");
const RUST_INHERIT_DECOMPOSE: &str = include_str!("../languages/rust/inherit_decompose.scm");

const TYPESCRIPT_ENTITIES: &str = include_str!("../languages/typescript/entities.scm");
const TYPESCRIPT_RELATIONS: &str = include_str!("../languages/typescript/relations.scm");
const TYPESCRIPT_INHERIT_DECOMPOSE: &str =
    include_str!("../languages/typescript/inherit_decompose.scm");

const JAVASCRIPT_ENTITIES: &str = include_str!("../languages/javascript/entities.scm");
const JAVASCRIPT_RELATIONS: &str = include_str!("../languages/javascript/relations.scm");

const GO_ENTITIES: &str = include_str!("../languages/go/entities.scm");
const GO_RELATIONS: &str = include_str!("../languages/go/relations.scm");
const GO_INHERIT_DECOMPOSE: &str = include_str!("../languages/go/inherit_decompose.scm");

const JAVA_ENTITIES: &str = include_str!("../languages/java/entities.scm");
const JAVA_RELATIONS: &str = include_str!("../languages/java/relations.scm");
const JAVA_INHERIT_DECOMPOSE: &str = include_str!("../languages/java/inherit_decompose.scm");

const C_ENTITIES: &str = include_str!("../languages/c/entities.scm");
const C_RELATIONS: &str = include_str!("../languages/c/relations.scm");

const CPP_ENTITIES: &str = include_str!("../languages/cpp/entities.scm");
const CPP_RELATIONS: &str = include_str!("../languages/cpp/relations.scm");

const RUBY_ENTITIES: &str = include_str!("../languages/ruby/entities.scm");
const RUBY_RELATIONS: &str = include_str!("../languages/ruby/relations.scm");

const PHP_ENTITIES: &str = include_str!("../languages/php/entities.scm");
const PHP_RELATIONS: &str = include_str!("../languages/php/relations.scm");

const SWIFT_ENTITIES: &str = include_str!("../languages/swift/entities.scm");
const SWIFT_RELATIONS: &str = include_str!("../languages/swift/relations.scm");
const SWIFT_INHERIT_DECOMPOSE: &str = include_str!("../languages/swift/inherit_decompose.scm");

const KOTLIN_ENTITIES: &str = include_str!("../languages/kotlin/entities.scm");
const KOTLIN_RELATIONS: &str = include_str!("../languages/kotlin/relations.scm");
const KOTLIN_INHERIT_DECOMPOSE: &str = include_str!("../languages/kotlin/inherit_decompose.scm");

const CSHARP_ENTITIES: &str = include_str!("../languages/csharp/entities.scm");
const CSHARP_RELATIONS: &str = include_str!("../languages/csharp/relations.scm");

const SCALA_ENTITIES: &str = include_str!("../languages/scala/entities.scm");
const SCALA_RELATIONS: &str = include_str!("../languages/scala/relations.scm");

const LUA_ENTITIES: &str = include_str!("../languages/lua/entities.scm");
const LUA_RELATIONS: &str = include_str!("../languages/lua/relations.scm");

const ZIG_ENTITIES: &str = include_str!("../languages/zig/entities.scm");
const ZIG_RELATIONS: &str = include_str!("../languages/zig/relations.scm");

const ELIXIR_ENTITIES: &str = include_str!("../languages/elixir/entities.scm");
const ELIXIR_RELATIONS: &str = include_str!("../languages/elixir/relations.scm");

const DART_ENTITIES: &str = include_str!("../languages/dart/entities.scm");
const DART_RELATIONS: &str = include_str!("../languages/dart/relations.scm");

const OBJC_ENTITIES: &str = include_str!("../languages/objc/entities.scm");
const OBJC_RELATIONS: &str = include_str!("../languages/objc/relations.scm");

const HASKELL_ENTITIES: &str = include_str!("../languages/haskell/entities.scm");
const HASKELL_RELATIONS: &str = include_str!("../languages/haskell/relations.scm");

const PERL_ENTITIES: &str = include_str!("../languages/perl/entities.scm");
const PERL_RELATIONS: &str = include_str!("../languages/perl/relations.scm");

const R_ENTITIES: &str = include_str!("../languages/r/entities.scm");
const R_RELATIONS: &str = include_str!("../languages/r/relations.scm");

const OCAML_ENTITIES: &str = include_str!("../languages/ocaml/entities.scm");
const OCAML_RELATIONS: &str = include_str!("../languages/ocaml/relations.scm");

const BASH_ENTITIES: &str = include_str!("../languages/bash/entities.scm");
const BASH_RELATIONS: &str = include_str!("../languages/bash/relations.scm");

const SQL_ENTITIES: &str = include_str!("../languages/sql/entities.scm");
const SQL_RELATIONS: &str = include_str!("../languages/sql/relations.scm");

const JULIA_ENTITIES: &str = include_str!("../languages/julia/entities.scm");
const JULIA_RELATIONS: &str = include_str!("../languages/julia/relations.scm");

const PROTO_ENTITIES: &str = include_str!("../languages/proto/entities.scm");
const PROTO_RELATIONS: &str = include_str!("../languages/proto/relations.scm");

const POWERSHELL_ENTITIES: &str = include_str!("../languages/powershell/entities.scm");
const POWERSHELL_RELATIONS: &str = include_str!("../languages/powershell/relations.scm");

const VERILOG_ENTITIES: &str = include_str!("../languages/verilog/entities.scm");
const VERILOG_RELATIONS: &str = include_str!("../languages/verilog/relations.scm");

const HCL_ENTITIES: &str = include_str!("../languages/hcl/entities.scm");
const HCL_RELATIONS: &str = include_str!("../languages/hcl/relations.scm");

const TOML_ENTITIES: &str = include_str!("../languages/toml/entities.scm");
const TOML_RELATIONS: &str = include_str!("../languages/toml/relations.scm");

const YAML_ENTITIES: &str = include_str!("../languages/yaml/entities.scm");
const YAML_RELATIONS: &str = include_str!("../languages/yaml/relations.scm");

const ERLANG_ENTITIES: &str = include_str!("../languages/erlang/entities.scm");
const ERLANG_RELATIONS: &str = include_str!("../languages/erlang/relations.scm");

const DOCKERFILE_ENTITIES: &str = include_str!("../languages/dockerfile/entities.scm");
const DOCKERFILE_RELATIONS: &str = include_str!("../languages/dockerfile/relations.scm");

const FORTRAN_ENTITIES: &str = include_str!("../languages/fortran/entities.scm");
const FORTRAN_RELATIONS: &str = include_str!("../languages/fortran/relations.scm");

const NIX_ENTITIES: &str = include_str!("../languages/nix/entities.scm");
const NIX_RELATIONS: &str = include_str!("../languages/nix/relations.scm");

const SVELTE_ENTITIES: &str = include_str!("../languages/svelte/entities.scm");
const SVELTE_RELATIONS: &str = include_str!("../languages/svelte/relations.scm");

const FSHARP_ENTITIES: &str = include_str!("../languages/fsharp/entities.scm");
const FSHARP_RELATIONS: &str = include_str!("../languages/fsharp/relations.scm");

const GROOVY_ENTITIES: &str = include_str!("../languages/groovy/entities.scm");
const GROOVY_RELATIONS: &str = include_str!("../languages/groovy/relations.scm");

const CSS_ENTITIES: &str = include_str!("../languages/css/entities.scm");
const CSS_RELATIONS: &str = include_str!("../languages/css/relations.scm");

const HTML_ENTITIES: &str = include_str!("../languages/html/entities.scm");
const HTML_RELATIONS: &str = include_str!("../languages/html/relations.scm");

const JSON_ENTITIES: &str = include_str!("../languages/json/entities.scm");
const JSON_RELATIONS: &str = include_str!("../languages/json/relations.scm");

const XML_ENTITIES: &str = include_str!("../languages/xml/entities.scm");
const XML_RELATIONS: &str = include_str!("../languages/xml/relations.scm");

const MAKEFILE_ENTITIES: &str = include_str!("../languages/makefile/entities.scm");
const MAKEFILE_RELATIONS: &str = include_str!("../languages/makefile/relations.scm");

const CMAKE_ENTITIES: &str = include_str!("../languages/cmake/entities.scm");
const CMAKE_RELATIONS: &str = include_str!("../languages/cmake/relations.scm");

const GRAPHQL_ENTITIES: &str = include_str!("../languages/graphql/entities.scm");
const GRAPHQL_RELATIONS: &str = include_str!("../languages/graphql/relations.scm");

const GLSL_ENTITIES: &str = include_str!("../languages/glsl/entities.scm");
const GLSL_RELATIONS: &str = include_str!("../languages/glsl/relations.scm");

const COMMONLISP_ENTITIES: &str = include_str!("../languages/commonlisp/entities.scm");
const COMMONLISP_RELATIONS: &str = include_str!("../languages/commonlisp/relations.scm");

const ELM_ENTITIES: &str = include_str!("../languages/elm/entities.scm");
const ELM_RELATIONS: &str = include_str!("../languages/elm/relations.scm");

const ELISP_ENTITIES: &str = include_str!("../languages/elisp/entities.scm");
const ELISP_RELATIONS: &str = include_str!("../languages/elisp/relations.scm");

const INI_ENTITIES: &str = include_str!("../languages/ini/entities.scm");
const INI_RELATIONS: &str = include_str!("../languages/ini/relations.scm");

const TSX_ENTITIES: &str = include_str!("../languages/tsx/entities.scm");
const TSX_RELATIONS: &str = include_str!("../languages/tsx/relations.scm");

const STARLARK_ENTITIES: &str = include_str!("../languages/starlark/entities.scm");
const STARLARK_RELATIONS: &str = include_str!("../languages/starlark/relations.scm");

const MATLAB_ENTITIES: &str = include_str!("../languages/matlab/entities.scm");
const MATLAB_RELATIONS: &str = include_str!("../languages/matlab/relations.scm");

const MARKDOWN_ENTITIES: &str = include_str!("../languages/markdown/entities.scm");
const MARKDOWN_RELATIONS: &str = include_str!("../languages/markdown/relations.scm");

const CLOJURE_ENTITIES: &str = include_str!("../languages/clojure/entities.scm");
const CLOJURE_RELATIONS: &str = include_str!("../languages/clojure/relations.scm");

const CUDA_ENTITIES: &str = include_str!("../languages/cuda/entities.scm");
const CUDA_RELATIONS: &str = include_str!("../languages/cuda/relations.scm");

const PASCAL_ENTITIES: &str = include_str!("../languages/pascal/entities.scm");
const PASCAL_RELATIONS: &str = include_str!("../languages/pascal/relations.scm");

const VB6_ENTITIES: &str = include_str!("../languages/vb6/entities.scm");
const VB6_RELATIONS: &str = include_str!("../languages/vb6/relations.scm");

/// Create a registry with all bundled language packs.
pub fn bundled_registry() -> Result<LanguageRegistry> {
    let mut registry = LanguageRegistry::new();

    registry.register(python_pack()?);

    // These may fail if queries don't match the grammar — log and skip
    match rust_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Rust language pack: {e}"),
    }
    match typescript_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load TypeScript language pack: {e}"),
    }
    if let Ok(pack) = javascript_pack() {
        registry.register(pack);
    } else {
        eprintln!("warning: failed to load JavaScript language pack");
    }
    if let Ok(pack) = go_pack() {
        registry.register(pack);
    } else {
        eprintln!("warning: failed to load Go language pack");
    }
    if let Ok(pack) = java_pack() {
        registry.register(pack);
    } else {
        eprintln!("warning: failed to load Java language pack");
    }
    match c_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load C language pack: {e}"),
    }
    match cpp_pack() {
        // `.h` is claimed by the C pack by extension, but in a C++ codebase the
        // overwhelming majority of headers are C++ (templates, classes,
        // namespaces) — parsing those with the C grammar silently drops every
        // class, template, and namespace symbol in them. Probe the content and
        // route C++-looking headers to the C++ grammar instead.
        Ok(pack) => registry.register_with_content_probe(pack, &[".h"], is_cpp_header),
        Err(e) => eprintln!("warning: failed to load C++ language pack: {e}"),
    }
    match ruby_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Ruby language pack: {e}"),
    }
    match php_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load PHP language pack: {e}"),
    }
    match swift_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Swift language pack: {e}"),
    }
    match kotlin_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Kotlin language pack: {e}"),
    }
    match csharp_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load C# language pack: {e}"),
    }
    match scala_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Scala language pack: {e}"),
    }
    match lua_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Lua language pack: {e}"),
    }
    match zig_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Zig language pack: {e}"),
    }
    match elixir_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Elixir language pack: {e}"),
    }
    match dart_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Dart language pack: {e}"),
    }
    match objc_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Objective-C language pack: {e}"),
    }
    match haskell_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Haskell language pack: {e}"),
    }
    match perl_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Perl language pack: {e}"),
    }
    match r_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load R language pack: {e}"),
    }
    match ocaml_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load OCaml language pack: {e}"),
    }
    match bash_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Bash language pack: {e}"),
    }
    match sql_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load SQL language pack: {e}"),
    }
    match julia_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Julia language pack: {e}"),
    }
    match proto_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Protobuf language pack: {e}"),
    }
    match powershell_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load PowerShell language pack: {e}"),
    }
    match verilog_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Verilog language pack: {e}"),
    }
    match hcl_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load HCL language pack: {e}"),
    }
    match toml_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load TOML language pack: {e}"),
    }
    match yaml_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load YAML language pack: {e}"),
    }
    match erlang_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Erlang language pack: {e}"),
    }
    match dockerfile_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Dockerfile language pack: {e}"),
    }
    match fortran_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Fortran language pack: {e}"),
    }
    match nix_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Nix language pack: {e}"),
    }
    match svelte_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Svelte language pack: {e}"),
    }
    match fsharp_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load F# language pack: {e}"),
    }
    match groovy_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Groovy language pack: {e}"),
    }
    match css_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load CSS language pack: {e}"),
    }
    match html_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load HTML language pack: {e}"),
    }
    match json_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load JSON language pack: {e}"),
    }
    match xml_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load XML language pack: {e}"),
    }
    match makefile_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Makefile language pack: {e}"),
    }
    match cmake_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load CMake language pack: {e}"),
    }
    match graphql_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load GraphQL language pack: {e}"),
    }
    match glsl_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load GLSL language pack: {e}"),
    }
    match commonlisp_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Common Lisp language pack: {e}"),
    }
    match elm_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Elm language pack: {e}"),
    }
    match elisp_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Emacs Lisp language pack: {e}"),
    }
    match ini_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load INI language pack: {e}"),
    }
    match tsx_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load TSX language pack: {e}"),
    }
    match starlark_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Starlark language pack: {e}"),
    }
    match matlab_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load MATLAB language pack: {e}"),
    }
    match markdown_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Markdown language pack: {e}"),
    }
    match clojure_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Clojure language pack: {e}"),
    }
    match cuda_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load CUDA language pack: {e}"),
    }
    match pascal_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load Pascal/Delphi language pack: {e}"),
    }
    match vb6_pack() {
        Ok(pack) => registry.register(pack),
        Err(e) => eprintln!("warning: failed to load VB6 language pack: {e}"),
    }

    Ok(registry)
}

fn python_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_python::LANGUAGE.into();
    LanguagePack::new_with_custom_edges(
        "python",
        vec![".py"],
        grammar,
        PYTHON_ENTITIES,
        PYTHON_RELATIONS,
        vec![
            CustomEdgeDef {
                name: "DECORATED_BY".to_string(),
                capture: "decorates".to_string(),
            },
            CustomEdgeDef {
                name: "REGISTERS_MIDDLEWARE".to_string(),
                capture: "middleware".to_string(),
            },
            CustomEdgeDef {
                name: "INJECTS_DEPENDENCY".to_string(),
                capture: "depends".to_string(),
            },
        ],
    )?
    .with_inherit_decompose(PYTHON_INHERIT_DECOMPOSE)
}

fn rust_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_rust::LANGUAGE.into();
    LanguagePack::new("rust", vec![".rs"], grammar, RUST_ENTITIES, RUST_RELATIONS)?
        .with_inherit_decompose(RUST_INHERIT_DECOMPOSE)
}

fn typescript_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
    LanguagePack::new(
        "typescript",
        vec![".ts"],
        grammar,
        TYPESCRIPT_ENTITIES,
        TYPESCRIPT_RELATIONS,
    )?
    .with_inherit_decompose(TYPESCRIPT_INHERIT_DECOMPOSE)
}

fn javascript_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_javascript::LANGUAGE.into();
    LanguagePack::new(
        "javascript",
        vec![".js", ".jsx", ".mjs"],
        grammar,
        JAVASCRIPT_ENTITIES,
        JAVASCRIPT_RELATIONS,
    )
}

fn go_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_go::LANGUAGE.into();
    LanguagePack::new_with_custom_edges(
        "go",
        vec![".go"],
        grammar,
        GO_ENTITIES,
        GO_RELATIONS,
        vec![CustomEdgeDef {
            name: "SPAWNS".to_string(),
            capture: "goroutine".to_string(),
        }],
    )?
    .with_inherit_decompose(GO_INHERIT_DECOMPOSE)
}

fn java_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_java::LANGUAGE.into();
    LanguagePack::new(
        "java",
        vec![".java"],
        grammar,
        JAVA_ENTITIES,
        JAVA_RELATIONS,
    )?
    .with_inherit_decompose(JAVA_INHERIT_DECOMPOSE)
}

fn c_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_c::LANGUAGE.into();
    LanguagePack::new("c", vec![".c", ".h"], grammar, C_ENTITIES, C_RELATIONS)
}

/// Content probe for `.h` files: true when the header uses C++-only syntax.
///
/// `.h` is ambiguous — valid for both C and C++ — and extension lookup alone
/// sends every one of them to the C grammar, which cannot represent classes,
/// templates, or namespaces. Each marker below is a construct that does not
/// parse as C, so a match means the file must be treated as C++. Plain C
/// headers match none of them and keep the C grammar.
fn is_cpp_header(content: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(content) else {
        return false;
    };
    const CPP_MARKERS: &[&str] = &[
        "template<",
        "template <",
        "namespace ",
        "class ",
        "public:",
        "private:",
        "protected:",
        "virtual ",
        "std::",
        "extern \"C\"",
    ];
    CPP_MARKERS.iter().any(|m| text.contains(m))
}

fn cpp_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_cpp::LANGUAGE.into();
    LanguagePack::new(
        "cpp",
        vec![".cpp", ".cc", ".cxx", ".hpp", ".hxx", ".hh"],
        grammar,
        CPP_ENTITIES,
        CPP_RELATIONS,
    )
}

fn ruby_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_ruby::LANGUAGE.into();
    LanguagePack::new(
        "ruby",
        vec![".rb", ".rake", ".gemspec"],
        grammar,
        RUBY_ENTITIES,
        RUBY_RELATIONS,
    )
}

fn php_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_php::LANGUAGE_PHP.into();
    LanguagePack::new("php", vec![".php"], grammar, PHP_ENTITIES, PHP_RELATIONS)
}

fn swift_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_swift::LANGUAGE.into();
    LanguagePack::new(
        "swift",
        vec![".swift"],
        grammar,
        SWIFT_ENTITIES,
        SWIFT_RELATIONS,
    )?
    .with_inherit_decompose(SWIFT_INHERIT_DECOMPOSE)
}

fn kotlin_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_kotlin_ng::LANGUAGE.into();
    LanguagePack::new(
        "kotlin",
        vec![".kt", ".kts"],
        grammar,
        KOTLIN_ENTITIES,
        KOTLIN_RELATIONS,
    )?
    .with_inherit_decompose(KOTLIN_INHERIT_DECOMPOSE)
}

fn csharp_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_c_sharp::LANGUAGE.into();
    LanguagePack::new(
        "csharp",
        vec![".cs"],
        grammar,
        CSHARP_ENTITIES,
        CSHARP_RELATIONS,
    )
}

fn scala_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_scala::LANGUAGE.into();
    LanguagePack::new(
        "scala",
        vec![".scala", ".sc"],
        grammar,
        SCALA_ENTITIES,
        SCALA_RELATIONS,
    )
}

fn lua_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_lua::LANGUAGE.into();
    LanguagePack::new("lua", vec![".lua"], grammar, LUA_ENTITIES, LUA_RELATIONS)
}

fn zig_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_zig::LANGUAGE.into();
    LanguagePack::new("zig", vec![".zig"], grammar, ZIG_ENTITIES, ZIG_RELATIONS)
}

fn elixir_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_elixir::LANGUAGE.into();
    LanguagePack::new(
        "elixir",
        vec![".ex", ".exs"],
        grammar,
        ELIXIR_ENTITIES,
        ELIXIR_RELATIONS,
    )
}

fn dart_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_dart::LANGUAGE.into();
    LanguagePack::new(
        "dart",
        vec![".dart"],
        grammar,
        DART_ENTITIES,
        DART_RELATIONS,
    )
}

fn objc_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_objc::LANGUAGE.into();
    LanguagePack::new(
        "objc",
        vec![".m", ".mm"],
        grammar,
        OBJC_ENTITIES,
        OBJC_RELATIONS,
    )
}

fn haskell_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_haskell::LANGUAGE.into();
    LanguagePack::new(
        "haskell",
        vec![".hs", ".lhs"],
        grammar,
        HASKELL_ENTITIES,
        HASKELL_RELATIONS,
    )
}

fn perl_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_perl::LANGUAGE.into();
    LanguagePack::new(
        "perl",
        vec![".pl", ".pm", ".t"],
        grammar,
        PERL_ENTITIES,
        PERL_RELATIONS,
    )
}

fn r_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_r::LANGUAGE.into();
    LanguagePack::new(
        "r",
        vec![".r", ".R", ".Rmd"],
        grammar,
        R_ENTITIES,
        R_RELATIONS,
    )
}

fn ocaml_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_ocaml::LANGUAGE_OCAML.into();
    LanguagePack::new(
        "ocaml",
        vec![".ml", ".mli"],
        grammar,
        OCAML_ENTITIES,
        OCAML_RELATIONS,
    )
}

fn bash_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_bash::LANGUAGE.into();
    LanguagePack::new(
        "bash",
        vec![".sh", ".bash", ".zsh"],
        grammar,
        BASH_ENTITIES,
        BASH_RELATIONS,
    )
}

fn sql_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_sequel::LANGUAGE.into();
    LanguagePack::new("sql", vec![".sql"], grammar, SQL_ENTITIES, SQL_RELATIONS)
}

fn julia_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_julia::LANGUAGE.into();
    LanguagePack::new(
        "julia",
        vec![".jl"],
        grammar,
        JULIA_ENTITIES,
        JULIA_RELATIONS,
    )
}

fn proto_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_proto::LANGUAGE.into();
    LanguagePack::new(
        "proto",
        vec![".proto"],
        grammar,
        PROTO_ENTITIES,
        PROTO_RELATIONS,
    )
}

fn powershell_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_powershell::LANGUAGE.into();
    LanguagePack::new(
        "powershell",
        vec![".ps1", ".psm1", ".psd1"],
        grammar,
        POWERSHELL_ENTITIES,
        POWERSHELL_RELATIONS,
    )
}

fn verilog_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_verilog::LANGUAGE.into();
    LanguagePack::new(
        "verilog",
        vec![".v", ".sv", ".svh", ".vh"],
        grammar,
        VERILOG_ENTITIES,
        VERILOG_RELATIONS,
    )
}

fn hcl_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_hcl::LANGUAGE.into();
    LanguagePack::new(
        "hcl",
        vec![".hcl", ".tf", ".tfvars"],
        grammar,
        HCL_ENTITIES,
        HCL_RELATIONS,
    )
}

fn toml_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_toml_ng::LANGUAGE.into();
    LanguagePack::new(
        "toml",
        vec![".toml"],
        grammar,
        TOML_ENTITIES,
        TOML_RELATIONS,
    )
}

fn yaml_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_yaml::LANGUAGE.into();
    LanguagePack::new(
        "yaml",
        vec![".yml", ".yaml"],
        grammar,
        YAML_ENTITIES,
        YAML_RELATIONS,
    )
}

fn erlang_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_erlang::LANGUAGE.into();
    LanguagePack::new(
        "erlang",
        vec![".erl", ".hrl"],
        grammar,
        ERLANG_ENTITIES,
        ERLANG_RELATIONS,
    )
}

fn dockerfile_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_containerfile::LANGUAGE.into();
    LanguagePack::new(
        "dockerfile",
        vec!["Dockerfile", "Containerfile", ".dockerfile"],
        grammar,
        DOCKERFILE_ENTITIES,
        DOCKERFILE_RELATIONS,
    )
}

fn fortran_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_fortran::LANGUAGE.into();
    LanguagePack::new(
        "fortran",
        vec![".f90", ".f95", ".f03", ".f08", ".f", ".for"],
        grammar,
        FORTRAN_ENTITIES,
        FORTRAN_RELATIONS,
    )
}

fn nix_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_nix::LANGUAGE.into();
    LanguagePack::new("nix", vec![".nix"], grammar, NIX_ENTITIES, NIX_RELATIONS)
}

fn svelte_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_svelte_ng::LANGUAGE.into();
    LanguagePack::new(
        "svelte",
        vec![".svelte"],
        grammar,
        SVELTE_ENTITIES,
        SVELTE_RELATIONS,
    )
}

fn fsharp_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_fsharp::LANGUAGE_FSHARP.into();
    LanguagePack::new(
        "fsharp",
        vec![".fs", ".fsi", ".fsx"],
        grammar,
        FSHARP_ENTITIES,
        FSHARP_RELATIONS,
    )
}

fn groovy_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_groovy::LANGUAGE.into();
    LanguagePack::new(
        "groovy",
        vec![".groovy", ".gradle"],
        grammar,
        GROOVY_ENTITIES,
        GROOVY_RELATIONS,
    )
}

fn css_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_css::LANGUAGE.into();
    LanguagePack::new("css", vec![".css"], grammar, CSS_ENTITIES, CSS_RELATIONS)
}

fn html_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_html::LANGUAGE.into();
    LanguagePack::new(
        "html",
        vec![".html", ".htm"],
        grammar,
        HTML_ENTITIES,
        HTML_RELATIONS,
    )
}

fn json_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_json::LANGUAGE.into();
    LanguagePack::new(
        "json",
        vec![".json"],
        grammar,
        JSON_ENTITIES,
        JSON_RELATIONS,
    )
}

fn xml_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_xml::LANGUAGE_XML.into();
    LanguagePack::new(
        "xml",
        vec![".xml", ".xsl", ".xsd", ".svg", ".plist"],
        grammar,
        XML_ENTITIES,
        XML_RELATIONS,
    )
}

fn makefile_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_make::LANGUAGE.into();
    LanguagePack::new(
        "makefile",
        vec!["Makefile", "makefile", "GNUmakefile", ".mk"],
        grammar,
        MAKEFILE_ENTITIES,
        MAKEFILE_RELATIONS,
    )
}

fn cmake_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_cmake::LANGUAGE.into();
    LanguagePack::new(
        "cmake",
        vec!["CMakeLists.txt", ".cmake"],
        grammar,
        CMAKE_ENTITIES,
        CMAKE_RELATIONS,
    )
}

fn graphql_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_graphql::LANGUAGE.into();
    LanguagePack::new(
        "graphql",
        vec![".graphql", ".gql"],
        grammar,
        GRAPHQL_ENTITIES,
        GRAPHQL_RELATIONS,
    )
}

fn glsl_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_glsl::LANGUAGE_GLSL.into();
    LanguagePack::new(
        "glsl",
        vec![".glsl", ".vert", ".frag", ".geom", ".comp"],
        grammar,
        GLSL_ENTITIES,
        GLSL_RELATIONS,
    )
}

fn commonlisp_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_commonlisp::LANGUAGE_COMMONLISP.into();
    LanguagePack::new(
        "commonlisp",
        vec![".lisp", ".lsp", ".cl", ".asd"],
        grammar,
        COMMONLISP_ENTITIES,
        COMMONLISP_RELATIONS,
    )
}

fn elm_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_elm::LANGUAGE.into();
    LanguagePack::new("elm", vec![".elm"], grammar, ELM_ENTITIES, ELM_RELATIONS)
}

fn elisp_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_elisp::LANGUAGE.into();
    LanguagePack::new(
        "elisp",
        vec![".el"],
        grammar,
        ELISP_ENTITIES,
        ELISP_RELATIONS,
    )
}

fn ini_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_ini::LANGUAGE.into();
    LanguagePack::new(
        "ini",
        vec![".ini", ".cfg", ".conf"],
        grammar,
        INI_ENTITIES,
        INI_RELATIONS,
    )
}

fn tsx_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_typescript::LANGUAGE_TSX.into();
    LanguagePack::new("tsx", vec![".tsx"], grammar, TSX_ENTITIES, TSX_RELATIONS)
}

fn starlark_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_starlark::LANGUAGE.into();
    LanguagePack::new(
        "starlark",
        vec![".bzl", ".star", "BUILD", "BUILD.bazel", "WORKSPACE"],
        grammar,
        STARLARK_ENTITIES,
        STARLARK_RELATIONS,
    )
}

fn matlab_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_matlab::LANGUAGE.into();
    LanguagePack::new(
        "matlab",
        vec![".mlx", ".mat"],
        grammar,
        MATLAB_ENTITIES,
        MATLAB_RELATIONS,
    )
}

fn markdown_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_md::LANGUAGE.into();
    LanguagePack::new(
        "markdown",
        vec![".md", ".markdown"],
        grammar,
        MARKDOWN_ENTITIES,
        MARKDOWN_RELATIONS,
    )
}

fn clojure_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_clojure_orchard::LANGUAGE.into();
    LanguagePack::new(
        "clojure",
        vec![".clj", ".cljs", ".cljc", ".edn"],
        grammar,
        CLOJURE_ENTITIES,
        CLOJURE_RELATIONS,
    )
}

fn cuda_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_cuda::LANGUAGE.into();
    LanguagePack::new(
        "cuda",
        vec![".cu", ".cuh"],
        grammar,
        CUDA_ENTITIES,
        CUDA_RELATIONS,
    )
}

fn pascal_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_pascal::LANGUAGE.into();
    LanguagePack::new(
        "pascal",
        vec![".pas", ".pp", ".dpr", ".dpk", ".inc", ".lpr"],
        grammar,
        PASCAL_ENTITIES,
        PASCAL_RELATIONS,
    )
}

fn vb6_pack() -> Result<LanguagePack> {
    let grammar = tree_sitter_vb6::language();
    LanguagePack::new(
        "vb6",
        vec![".bas", ".cls", ".frm"],
        grammar,
        VB6_ENTITIES,
        VB6_RELATIONS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_packs_load() {
        // Verify bundled_registry succeeds (individual packs may warn but shouldn't panic)
        let registry = bundled_registry().expect("bundled_registry should succeed");
        assert!(
            registry.languages().count() > 0,
            "registry should have at least one language"
        );
    }

    #[test]
    fn test_powershell_pack() {
        powershell_pack().expect("PowerShell pack should load");
    }

    #[test]
    fn test_verilog_pack() {
        verilog_pack().expect("Verilog pack should load");
    }

    #[test]
    fn test_hcl_pack() {
        hcl_pack().expect("HCL pack should load");
    }

    #[test]
    fn test_toml_pack() {
        toml_pack().expect("TOML pack should load");
    }

    #[test]
    fn test_yaml_pack() {
        yaml_pack().expect("YAML pack should load");
    }

    #[test]
    fn test_erlang_pack() {
        erlang_pack().expect("Erlang pack should load");
    }

    #[test]
    fn test_dockerfile_pack() {
        dockerfile_pack().expect("Dockerfile pack should load");
    }

    #[test]
    fn test_fortran_pack() {
        fortran_pack().expect("Fortran pack should load");
    }

    #[test]
    fn test_nix_pack() {
        nix_pack().expect("Nix pack should load");
    }

    #[test]
    fn test_svelte_pack() {
        svelte_pack().expect("Svelte pack should load");
    }

    #[test]
    fn test_fsharp_pack() {
        fsharp_pack().expect("F# pack should load");
    }

    #[test]
    fn test_groovy_pack() {
        groovy_pack().expect("Groovy pack should load");
    }

    #[test]
    fn test_css_pack() {
        css_pack().expect("CSS pack should load");
    }

    #[test]
    fn test_html_pack() {
        html_pack().expect("HTML pack should load");
    }

    #[test]
    fn test_json_pack() {
        json_pack().expect("JSON pack should load");
    }

    #[test]
    fn test_xml_pack() {
        xml_pack().expect("XML pack should load");
    }

    #[test]
    fn test_makefile_pack() {
        makefile_pack().expect("Makefile pack should load");
    }

    #[test]
    fn test_cmake_pack() {
        cmake_pack().expect("CMake pack should load");
    }

    #[test]
    fn test_graphql_pack() {
        graphql_pack().expect("GraphQL pack should load");
    }

    #[test]
    fn test_glsl_pack() {
        glsl_pack().expect("GLSL pack should load");
    }

    #[test]
    fn test_commonlisp_pack() {
        commonlisp_pack().expect("Common Lisp pack should load");
    }

    #[test]
    fn test_elm_pack() {
        elm_pack().expect("Elm pack should load");
    }

    #[test]
    fn test_elisp_pack() {
        elisp_pack().expect("Emacs Lisp pack should load");
    }

    #[test]
    fn test_ini_pack() {
        ini_pack().expect("INI pack should load");
    }

    #[test]
    fn test_tsx_pack() {
        tsx_pack().expect("TSX pack should load");
    }

    #[test]
    fn test_matlab_pack() {
        matlab_pack().expect("MATLAB pack should load");
    }

    #[test]
    fn test_markdown_pack() {
        markdown_pack().expect("Markdown pack should load");
    }

    #[test]
    fn test_clojure_pack() {
        clojure_pack().expect("Clojure pack should load");
    }

    #[test]
    fn test_cuda_pack() {
        cuda_pack().expect("CUDA pack should load");
    }

    #[test]
    fn test_vb6_pack() {
        vb6_pack().expect("VB6 pack should load");
    }

    #[test]
    fn test_vb6_e2e_smoke() {
        let pack = vb6_pack().expect("VB6 pack should load");
        // Note: the tree-sitter-vb6 grammar parses `Call Foo(args)` incorrectly (Call becomes
        // the function name); use direct call syntax `Foo(args)` instead.
        let src = r#"VERSION 1.0 CLASS
BEGIN
  MultiUse = -1
END
Attribute VB_Name = "TestClass"
Option Explicit

Private mName As String

Public Sub Initialize(name As String)
    mName = name
End Sub

Public Function GetName() As String
    GetName = mName
End Function

Private Sub Helper()
    Dim result As String
    result = GetName()
    Initialize(result)
End Sub
"#;
        let extraction =
            infigraph_core::extract::extract_file("TestClass.cls", src.as_bytes(), &pack)
                .expect("extract_file should succeed");

        let symbol_names: Vec<&str> = extraction.symbols.iter().map(|s| s.name.as_str()).collect();
        println!("Symbols: {:?}", symbol_names);
        println!(
            "Symbol kinds: {:?}",
            extraction
                .symbols
                .iter()
                .map(|s| format!("{} ({:?})", s.name, s.kind))
                .collect::<Vec<_>>()
        );
        println!(
            "Relations: {:?}",
            extraction
                .relations
                .iter()
                .map(|r| format!("{} -> {}", r.source_id, r.target_id))
                .collect::<Vec<_>>()
        );

        // Verify module symbol
        let module = extraction.symbols.iter().find(|s| s.name == "TestClass");
        assert!(
            module.is_some(),
            "Expected Module symbol 'TestClass', got: {:?}",
            symbol_names
        );
        assert_eq!(
            module.unwrap().kind,
            infigraph_core::model::SymbolKind::Module
        );

        // Verify functions/subs
        assert!(
            symbol_names.contains(&"Initialize"),
            "Expected 'Initialize'"
        );
        assert!(symbol_names.contains(&"GetName"), "Expected 'GetName'");
        assert!(symbol_names.contains(&"Helper"), "Expected 'Helper'");

        // Verify variable
        assert!(symbol_names.contains(&"mName"), "Expected 'mName'");

        // Verify call relations exist — Helper calls GetName and Initialize
        assert!(
            !extraction.relations.is_empty(),
            "Expected call relations from Helper"
        );

        let relation_pairs: Vec<(&str, &str)> = extraction
            .relations
            .iter()
            .map(|r| (r.source_id.as_str(), r.target_id.as_str()))
            .collect();
        println!("Relation pairs: {:?}", relation_pairs);

        let helper_calls_getname = extraction
            .relations
            .iter()
            .any(|r| r.source_id.ends_with("::Helper") && r.target_id.ends_with("::GetName"));
        let helper_calls_initialize = extraction
            .relations
            .iter()
            .any(|r| r.source_id.ends_with("::Helper") && r.target_id.ends_with("::Initialize"));

        assert!(
            helper_calls_getname,
            "Expected Helper -> GetName call edge, got: {:?}",
            relation_pairs
        );
        assert!(
            helper_calls_initialize,
            "Expected Helper -> Initialize call edge, got: {:?}",
            relation_pairs
        );
    }

    #[test]
    fn test_sql_table_lineage() {
        let pack = sql_pack().unwrap();
        let sql = b"CREATE TABLE output AS SELECT col1 FROM source_a INNER JOIN source_b ON source_a.id = source_b.id;
WITH cte1 AS (SELECT * FROM base_table), cte2 AS (SELECT * FROM cte1) SELECT * FROM cte2;
INSERT INTO target_table SELECT * FROM input_table;";

        let extraction = infigraph_core::extract::extract_file("test.sql", sql, &pack).unwrap();

        let sym_names: Vec<&str> = extraction.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            sym_names.contains(&"output"),
            "expected CREATE TABLE output"
        );
        assert!(sym_names.contains(&"cte1"), "expected CTE cte1");
        assert!(sym_names.contains(&"cte2"), "expected CTE cte2");

        let has_edge = |src_suffix: &str, tgt_suffix: &str| {
            extraction
                .relations
                .iter()
                .any(|r| r.source_id.ends_with(src_suffix) && r.target_id.ends_with(tgt_suffix))
        };
        assert!(
            has_edge("::output", "::source_a"),
            "expected output -> source_a"
        );
        assert!(
            has_edge("::output", "::source_b"),
            "expected output -> source_b"
        );
        assert!(
            has_edge("::cte1", "::base_table"),
            "expected cte1 -> base_table"
        );
        assert!(has_edge("::cte2", "::cte1"), "expected cte2 -> cte1");
    }

    #[test]
    fn test_sql_spark_extraction_real() {
        let sample_dir = std::path::Path::new("/tmp/efp-group-sql-samples");
        if !sample_dir.exists() {
            println!("skipping — samples not found");
            return;
        }
        let pack = sql_pack().unwrap();
        for entry in std::fs::read_dir(sample_dir).unwrap().flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("sql") {
                continue;
            }
            let Ok(src) = std::fs::read(&p) else {
                continue;
            };
            if src.is_empty() {
                continue;
            }
            let fname = p.file_name().unwrap().to_string_lossy().to_string();
            match infigraph_core::extract::extract_file(&fname, &src, &pack) {
                Ok(e) => println!(
                    "{fname}: {} symbols, {} relations",
                    e.symbols.len(),
                    e.relations.len()
                ),
                Err(err) => println!("{fname}: EXTRACT ERROR: {err}"),
            }
        }
    }

    #[test]
    fn test_sql_notebook_file_level_refs() {
        let pack = sql_pack().unwrap();
        let sql = b"-- Databricks notebook source
SELECT * FROM fraud_360_rpt WHERE year='2023';

-- COMMAND ----------

SELECT a.*, b.score FROM risk_assessment a
LEFT JOIN risk_rules b ON a.id = b.assessment_id;";

        let extraction = infigraph_core::extract::extract_file("notebook.sql", sql, &pack).unwrap();

        assert!(
            !extraction.relations.is_empty(),
            "expected file-level relations from notebook SQL"
        );
        let has_edge = |tgt: &str| {
            extraction
                .relations
                .iter()
                .any(|r| r.target_id.ends_with(tgt))
        };
        assert!(has_edge("::fraud_360_rpt"), "expected ref to fraud_360_rpt");
        assert!(
            has_edge("::risk_assessment"),
            "expected ref to risk_assessment"
        );
        assert!(has_edge("::risk_rules"), "expected ref to risk_rules");

        let file_sourced = extraction
            .relations
            .iter()
            .any(|r| r.source_id.contains("notebook.sql"));
        assert!(
            file_sourced,
            "expected file-level source for notebook relations"
        );
    }

    #[test]
    fn test_sql_dialect_coverage() {
        let pack = sql_pack().unwrap();

        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, &[u8], &[&str], &[(&str, &str)])> = vec![
            // (label, sql, expected_targets, expected_edges as (src_suffix, tgt_suffix))

            // Spark SQL
            (
                "spark_ctas",
                b"CREATE TABLE output AS SELECT * FROM src_a JOIN src_b ON a.id = b.id;" as &[u8],
                &["src_a", "src_b"],
                &[("::output", "::src_a"), ("::output", "::src_b")],
            ),
            (
                "spark_insert_overwrite",
                b"INSERT OVERWRITE TABLE target SELECT * FROM source;",
                &["source"],
                &[],
            ),
            (
                "spark_cte",
                b"WITH stg AS (SELECT * FROM raw_events) SELECT * FROM stg;",
                &["raw_events", "stg"],
                &[("::stg", "::raw_events")],
            ),
            // T-SQL / SQL Server
            (
                "tsql_select_into",
                b"SELECT * INTO new_table FROM old_table;",
                &["old_table"],
                &[],
            ),
            (
                "tsql_cte_insert",
                b"WITH cte AS (SELECT * FROM src) INSERT INTO tgt SELECT * FROM cte;",
                &["src", "cte"],
                &[("::cte", "::src")],
            ),
            // PostgreSQL
            (
                "pg_ctas",
                b"CREATE TABLE summary AS SELECT dept, count(*) FROM employees GROUP BY dept;",
                &["employees"],
                &[("::summary", "::employees")],
            ),
            (
                "pg_with_insert",
                b"WITH src AS (SELECT * FROM raw) INSERT INTO clean SELECT * FROM src;",
                &["raw", "src"],
                &[("::src", "::raw")],
            ),
            // MySQL
            (
                "mysql_insert_ignore",
                b"INSERT IGNORE INTO users SELECT * FROM staging_users;",
                &["staging_users"],
                &[],
            ),
            // BigQuery style
            (
                "bq_create_or_replace",
                b"CREATE OR REPLACE TABLE output AS SELECT * FROM input;",
                &["input"],
                &[],
            ),
            // Common patterns
            (
                "subquery",
                b"SELECT * FROM (SELECT id FROM raw_data) sub;",
                &["raw_data"],
                &[],
            ),
            (
                "multi_join",
                b"SELECT * FROM a JOIN b ON a.id = b.id LEFT JOIN c ON b.id = c.id;",
                &["a", "b", "c"],
                &[],
            ),
            (
                "union_all",
                b"SELECT * FROM t1 UNION ALL SELECT * FROM t2;",
                &["t1", "t2"],
                &[],
            ),
        ];

        let mut failures = Vec::new();

        for (label, sql, expected_targets, expected_edges) in &cases {
            let extraction = infigraph_core::extract::extract_file("test.sql", sql, &pack).unwrap();
            let all_targets: Vec<&str> = extraction
                .relations
                .iter()
                .map(|r| r.target_id.as_str())
                .collect();

            for tgt in *expected_targets {
                let suffix = format!("::{}", tgt);
                if !extraction
                    .relations
                    .iter()
                    .any(|r| r.target_id.ends_with(&suffix))
                {
                    failures.push(format!(
                        "{}: missing target ref to {}, got {:?}",
                        label, tgt, all_targets
                    ));
                }
            }

            for (src_suf, tgt_suf) in *expected_edges {
                if !extraction
                    .relations
                    .iter()
                    .any(|r| r.source_id.ends_with(src_suf) && r.target_id.ends_with(tgt_suf))
                {
                    let edges: Vec<_> = extraction
                        .relations
                        .iter()
                        .map(|r| format!("{} -> {}", r.source_id, r.target_id))
                        .collect();
                    failures.push(format!(
                        "{}: missing edge {} -> {}, got {:?}",
                        label, src_suf, tgt_suf, edges
                    ));
                }
            }
        }

        if !failures.is_empty() {
            panic!("SQL dialect coverage failures:\n{}", failures.join("\n"));
        }
    }

    #[test]
    fn test_sql_notebook_formats() {
        let pack = sql_pack().unwrap();

        let cases: Vec<(&str, &[u8], &[&str])> = vec![
            // Databricks notebook
            ("databricks", b"-- Databricks notebook source\nSELECT * FROM table_a;\n-- COMMAND ----------\nSELECT * FROM table_b;" as &[u8],
             &["table_a", "table_b"]),

            // Jupyter-style: SQL cells are just raw SQL (no magic prefix in .sql export)
            ("jupyter_plain", b"SELECT * FROM dataset_1;\nSELECT * FROM dataset_2 JOIN dataset_3 ON d2.id = d3.id;",
             &["dataset_1", "dataset_2", "dataset_3"]),

            // Zeppelin notebook style (paragraph markers)
            ("zeppelin", b"%sql\nSELECT * FROM zep_table_1;\n\n%sql\nSELECT a.* FROM zep_table_2 a LEFT JOIN zep_table_3 b ON a.id = b.id;",
             &["zep_table_1", "zep_table_2", "zep_table_3"]),

            // dbt-style Jinja (jinja tags parse as errors but table refs in FROM survive)
            ("dbt_ref", b"SELECT * FROM {{ ref('stg_orders') }}\nJOIN raw_customers ON orders.cust_id = raw_customers.id;",
             &["raw_customers"]),

            // Mixed DDL + bare SELECT
            ("mixed", b"CREATE TABLE output AS SELECT * FROM src;\nSELECT * FROM standalone_ref;",
             &["src", "standalone_ref"]),
        ];

        let mut failures = Vec::new();

        for (label, sql, expected_targets) in &cases {
            let extraction =
                infigraph_core::extract::extract_file("notebook.sql", sql, &pack).unwrap();
            let all_targets: Vec<String> = extraction
                .relations
                .iter()
                .map(|r| r.target_id.clone())
                .collect();

            for tgt in *expected_targets {
                let suffix = format!("::{}", tgt);
                if !extraction
                    .relations
                    .iter()
                    .any(|r| r.target_id.ends_with(&suffix))
                {
                    failures.push(format!(
                        "{}: missing target ref to {}, got {:?}",
                        label, tgt, all_targets
                    ));
                }
            }

            let all_file_sourced = extraction
                .relations
                .iter()
                .filter(|r| !r.source_id.contains("::"))
                .count()
                == 0;
            if !all_file_sourced {
                let non_qualified: Vec<_> = extraction
                    .relations
                    .iter()
                    .filter(|r| !r.source_id.contains("::"))
                    .map(|r| r.source_id.as_str())
                    .collect();
                if !non_qualified.is_empty() {
                    failures.push(format!(
                        "{}: source_ids without '::': {:?}",
                        label, non_qualified
                    ));
                }
            }
        }

        if !failures.is_empty() {
            panic!("Notebook format failures:\n{}", failures.join("\n"));
        }
    }

    #[test]
    fn test_sql_complex_nested_and_dml() {
        let pack = sql_pack().unwrap();

        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, &[u8], &[&str], &[(&str, &str)])> = vec![
            // Nested subqueries in FROM
            ("nested_subquery",
             b"SELECT * FROM (SELECT id FROM (SELECT id FROM deep_source) inner_q) outer_q;" as &[u8],
             &["deep_source"], &[]),

            // CTE chain: cte1 -> raw, cte2 -> cte1, cte3 -> cte2 + extra
            ("cte_chain",
             b"WITH cte1 AS (SELECT * FROM raw_data), \
               cte2 AS (SELECT * FROM cte1), \
               cte3 AS (SELECT a.* FROM cte2 a JOIN dim_table b ON a.id = b.id) \
               SELECT * FROM cte3;",
             &["raw_data", "cte1", "cte2", "dim_table", "cte3"],
             &[("::cte1", "::raw_data"), ("::cte2", "::cte1"), ("::cte3", "::cte2"), ("::cte3", "::dim_table")]),

            // CREATE TABLE with CTE
            ("ctas_with_cte",
             b"CREATE TABLE final_output AS \
               WITH stg AS (SELECT * FROM staging_table) \
               SELECT * FROM stg JOIN lookup ON stg.key = lookup.key;",
             &["staging_table", "stg", "lookup"],
             &[("::stg", "::staging_table")]),

            // INSERT with subquery + JOIN
            ("insert_subquery_join",
             b"INSERT INTO target_table \
               SELECT a.*, b.score FROM \
               (SELECT * FROM base_facts) a \
               JOIN scoring_model b ON a.id = b.id;",
             &["base_facts", "scoring_model"],
             &[]),

            // Multiple DML in one file: CREATE + INSERT + SELECT
            ("multi_dml",
             b"CREATE TABLE dim_users AS SELECT * FROM raw_users; \
               INSERT INTO fact_orders SELECT * FROM staging_orders JOIN dim_users ON so.uid = du.uid; \
               SELECT * FROM fact_orders WHERE dt = '2024-01-01';",
             &["raw_users", "staging_orders", "dim_users", "fact_orders"],
             &[("::dim_users", "::raw_users")]),

            // Correlated subquery in WHERE
            ("correlated_subquery",
             b"SELECT * FROM orders o WHERE EXISTS (SELECT 1 FROM returns r WHERE r.order_id = o.id);",
             &["orders", "returns"], &[]),

            // UNION inside CTE
            ("cte_union",
             b"WITH combined AS ( \
                 SELECT id, amount FROM source_a \
                 UNION ALL \
                 SELECT id, amount FROM source_b \
               ) SELECT * FROM combined;",
             &["source_a", "source_b", "combined"],
             &[]),

            // Deeply nested: CTE -> subquery -> JOIN
            ("deep_nesting",
             b"WITH prep AS ( \
                 SELECT * FROM ( \
                     SELECT a.*, b.cat FROM raw_events a \
                     LEFT JOIN categories b ON a.cat_id = b.id \
                 ) sub WHERE sub.cat IS NOT NULL \
               ) \
               INSERT INTO clean_events SELECT * FROM prep;",
             &["raw_events", "categories", "prep"],
             &[("::prep", "::raw_events"), ("::prep", "::categories")]),

            // Multiple JOINs
            ("multi_join_4way",
             b"SELECT * FROM t1 \
               JOIN t2 ON t1.id = t2.id \
               LEFT JOIN t3 ON t2.id = t3.id \
               INNER JOIN t4 ON t3.id = t4.id;",
             &["t1", "t2", "t3", "t4"], &[]),

            // INSERT with multiple JOINs
            ("insert_multi_join",
             b"INSERT INTO report \
               SELECT f.*, d.name, p.category FROM fact_sales f \
               JOIN dim_date d ON f.date_id = d.id \
               JOIN dim_product p ON f.prod_id = p.id;",
             &["fact_sales", "dim_date", "dim_product"],
             &[("::report", "::fact_sales")]),
        ];

        let mut failures = Vec::new();

        for (label, sql, expected_targets, expected_edges) in &cases {
            let extraction = infigraph_core::extract::extract_file("test.sql", sql, &pack).unwrap();
            let all_targets: Vec<String> = extraction
                .relations
                .iter()
                .map(|r| r.target_id.clone())
                .collect();
            let all_edges: Vec<String> = extraction
                .relations
                .iter()
                .map(|r| format!("{} -> {}", r.source_id, r.target_id))
                .collect();

            for tgt in *expected_targets {
                let suffix = format!("::{}", tgt);
                if !extraction
                    .relations
                    .iter()
                    .any(|r| r.target_id.ends_with(&suffix))
                {
                    failures.push(format!(
                        "{}: missing target ref to '{}', targets: {:?}",
                        label, tgt, all_targets
                    ));
                }
            }

            for (src_suf, tgt_suf) in *expected_edges {
                if !extraction
                    .relations
                    .iter()
                    .any(|r| r.source_id.ends_with(src_suf) && r.target_id.ends_with(tgt_suf))
                {
                    failures.push(format!(
                        "{}: missing edge {} -> {}, edges: {:?}",
                        label, src_suf, tgt_suf, all_edges
                    ));
                }
            }
        }

        if !failures.is_empty() {
            panic!("Complex nested/DML failures:\n{}", failures.join("\n"));
        }
    }

    #[test]
    fn test_pascal_relations() {
        let pack = pascal_pack().expect("Pascal pack should load");
        let src = br#"unit Test;
interface
uses SysUtils, Classes;
type
  TMyClass = class(TObject)
    procedure DoStuff;
  end;
implementation
procedure TMyClass.DoStuff;
begin
  WriteLn('hello');
  SomeObj.Method(42);
  inherited Create;
end;
end."#;

        let extraction = infigraph_core::extract::extract_file("test.pas", &src[..], &pack)
            .expect("extract_file should succeed for Pascal");

        let calls: Vec<_> = extraction
            .relations
            .iter()
            .filter(|r| r.kind == infigraph_core::model::RelationKind::Calls)
            .collect();

        let imports: Vec<_> = extraction
            .relations
            .iter()
            .filter(|r| r.kind == infigraph_core::model::RelationKind::Imports)
            .collect();

        let inherits: Vec<_> = extraction
            .relations
            .iter()
            .filter(|r| r.kind == infigraph_core::model::RelationKind::Inherits)
            .collect();

        assert!(
            !calls.is_empty(),
            "Expected CALLS relations from Pascal code"
        );
        assert!(
            !imports.is_empty(),
            "Expected IMPORTS relations from Pascal code"
        );
        assert!(
            !inherits.is_empty(),
            "Expected INHERITS relations from Pascal code"
        );
    }

    #[test]
    fn test_csharp_inheritance() {
        let pack = csharp_pack().expect("C# pack should load");
        let src = br#"using System;
using System.Collections.Generic;

namespace Intuit.Applications.PTG.DatabaseService
{
    public class SqliteDataLayer : IDataLayer
    {
        public void AddNewClient(string name) { }
        public void DeleteClient(int id) { }
    }
}
"#;
        let extraction = infigraph_core::extract::extract_file("test.cs", &src[..], &pack)
            .expect("extract_file should succeed for C#");

        let inherits: Vec<_> = extraction
            .relations
            .iter()
            .filter(|r| r.kind == infigraph_core::model::RelationKind::Inherits)
            .collect();
        assert!(
            !inherits.is_empty(),
            "Expected INHERITS relations from C# class : interface"
        );
    }

    #[test]
    fn test_rust_test_extraction() {
        let pack = rust_pack().unwrap();
        let src = br#"
#[test]
fn test_add() {
    assert_eq!(1 + 1, 2);
}

pub fn public_fn() -> i32 {
    42
}

fn private_fn() {}

pub(crate) fn crate_fn() {}
"#;
        let extraction = infigraph_core::extract::extract_file("lib.rs", src, &pack).unwrap();
        let syms = &extraction.symbols;

        let test_add = syms.iter().find(|s| s.name == "test_add");
        assert!(test_add.is_some(), "test_add should be extracted");
        assert_eq!(
            test_add.unwrap().kind,
            infigraph_core::model::SymbolKind::Test,
            "test_add should have Test kind, got {:?}",
            test_add.unwrap().kind
        );

        let public_fn = syms.iter().find(|s| s.name == "public_fn");
        assert!(public_fn.is_some(), "public_fn should be extracted");
        assert_eq!(
            public_fn.unwrap().visibility.as_deref(),
            Some("pub"),
            "public_fn should have pub visibility, got {:?}",
            public_fn.unwrap().visibility
        );

        let private_fn = syms.iter().find(|s| s.name == "private_fn");
        assert!(private_fn.is_some(), "private_fn should be extracted");
        assert!(
            private_fn.unwrap().visibility.is_none(),
            "private_fn should have no visibility modifier"
        );

        let crate_fn = syms.iter().find(|s| s.name == "crate_fn");
        assert!(crate_fn.is_some(), "crate_fn should be extracted");
        assert_eq!(
            crate_fn.unwrap().visibility.as_deref(),
            Some("pub(crate)"),
            "crate_fn should have pub(crate) visibility, got {:?}",
            crate_fn.unwrap().visibility
        );
    }

    #[test]
    fn test_rust_tokio_test_extraction() {
        let pack = rust_pack().unwrap();
        let src = br#"
#[tokio::test]
async fn test_async_op() {
    let x = 1;
}
"#;
        let extraction = infigraph_core::extract::extract_file("lib.rs", src, &pack).unwrap();
        let test_fn = extraction
            .symbols
            .iter()
            .find(|s| s.name == "test_async_op");
        assert!(test_fn.is_some(), "test_async_op should be extracted");
        assert_eq!(
            test_fn.unwrap().kind,
            infigraph_core::model::SymbolKind::Test,
            "tokio::test should produce Test kind, got {:?}",
            test_fn.unwrap().kind
        );
    }
}
