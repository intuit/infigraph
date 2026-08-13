use infigraph_core::extract::extract_file;
use infigraph_core::lang::LanguagePack;
use infigraph_core::model::{RelationKind, SymbolKind};

const PYTHON_ENTITIES: &str = r#"
(module
  (function_definition
    name: (identifier) @func.name
    body: (block
      (expression_statement
        (string) @func.docstring)?)) @func.def)

(module
  (decorated_definition
    (decorator) @func.decorator
    definition: (function_definition
      name: (identifier) @func.name
      body: (block
        (expression_statement
          (string) @func.docstring)?)) @func.def))

(class_definition
  name: (identifier) @class.name
  body: (block
    (expression_statement
      (string) @class.docstring)?)) @class.def

(class_definition
  body: (block
    (function_definition
      name: (identifier) @method.name
      body: (block
        (expression_statement
          (string) @method.docstring)?)) @method.def))

(class_definition
  body: (block
    (decorated_definition
      (decorator) @method.decorator
      definition: (function_definition
        name: (identifier) @method.name
        body: (block
          (expression_statement
            (string) @method.docstring)?)) @method.def)))

(module
  (function_definition
    name: (identifier) @test.name
    (#match? @test.name "^test_")
    body: (block
      (expression_statement
        (string) @test.docstring)?)) @test.def)

(module
  (expression_statement
    (assignment
      left: (identifier) @var.name)) @var.def)
"#;

const PYTHON_RELATIONS: &str = r#"
(call
  function: (identifier) @call.func) @call.site

(call
  function: (attribute
    object: (_) @call.receiver
    attribute: (identifier) @call.func)) @call.site

(import_statement
  name: (dotted_name) @import.module)

(import_from_statement
  module_name: (dotted_name) @import.module)

(class_definition
  name: (identifier) @inherit.child
  superclasses: (argument_list
    (identifier) @inherit.parent))
"#;

fn python_pack() -> LanguagePack {
    let grammar = tree_sitter_python::LANGUAGE.into();
    LanguagePack::new(
        "python",
        vec![".py"],
        grammar,
        PYTHON_ENTITIES,
        PYTHON_RELATIONS,
    )
    .unwrap()
}

// ---------- extract_file end-to-end ----------

#[test]
fn test_extract_simple_function() {
    let src = b"def hello(name: str) -> str:\n    \"\"\"Greet someone.\"\"\"\n    return f'hello {name}'\n";
    let pack = python_pack();
    let ext = extract_file("hello.py", src, &pack).unwrap();

    assert_eq!(ext.file, "hello.py");
    assert_eq!(ext.language, "python");
    assert!(!ext.content_hash.is_empty());
    assert_eq!(ext.symbols.len(), 1);
    assert_eq!(ext.symbols[0].name, "hello");
    assert_eq!(ext.symbols[0].kind, SymbolKind::Function);
    assert!(ext.symbols[0].parameters.is_some());
    assert!(ext.symbols[0]
        .parameters
        .as_deref()
        .unwrap()
        .contains("name"));
    assert!(ext.symbols[0].return_type.is_some());
}

#[test]
fn test_extract_class_with_methods() {
    let src = b"class Animal:\n    \"\"\"Base animal.\"\"\"\n    def speak(self):\n        pass\n    def eat(self, food):\n        pass\n";
    let pack = python_pack();
    let ext = extract_file("animal.py", src, &pack).unwrap();

    let class = ext.symbols.iter().find(|s| s.kind == SymbolKind::Class);
    assert!(class.is_some());
    assert_eq!(class.unwrap().name, "Animal");

    let methods: Vec<&str> = ext
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Method)
        .map(|s| s.name.as_str())
        .collect();
    assert!(methods.contains(&"speak"));
    assert!(methods.contains(&"eat"));
}

#[test]
fn test_extract_test_functions() {
    let src = b"def test_addition():\n    assert 1 + 1 == 2\n\ndef test_subtraction():\n    assert 2 - 1 == 1\n\ndef helper():\n    return 42\n";
    let pack = python_pack();
    let ext = extract_file("test_math.py", src, &pack).unwrap();

    let tests: Vec<&str> = ext
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Test)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(tests.len(), 2);
    assert!(tests.contains(&"test_addition"));
    assert!(tests.contains(&"test_subtraction"));

    let funcs: Vec<&str> = ext
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Function)
        .map(|s| s.name.as_str())
        .collect();
    assert!(funcs.contains(&"helper"));
}

#[test]
fn test_extract_call_relations() {
    let src = b"def main():\n    helper()\n    obj.method()\n\ndef helper():\n    pass\n";
    let pack = python_pack();
    let ext = extract_file("calls.py", src, &pack).unwrap();

    let calls: Vec<&str> = ext
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::Calls)
        .map(|r| r.target_id.as_str())
        .collect();
    assert!(
        calls.iter().any(|t| t.contains("helper")),
        "should detect helper() call"
    );
    assert!(
        calls.iter().any(|t| t.contains("method")),
        "should detect obj.method() call"
    );
}

#[test]
fn test_extract_import_relations() {
    let src = b"import os\nfrom pathlib import Path\n\ndef work():\n    pass\n";
    let pack = python_pack();
    let ext = extract_file("imports.py", src, &pack).unwrap();

    let imports: Vec<&str> = ext
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::Imports)
        .map(|r| r.target_id.as_str())
        .collect();
    assert!(
        imports.iter().any(|t| t.contains("os")),
        "should detect import os"
    );
    assert!(
        imports.iter().any(|t| t.contains("pathlib")),
        "should detect from pathlib import"
    );
}

#[test]
fn test_extract_inheritance() {
    let src = b"class Base:\n    pass\n\nclass Child(Base):\n    pass\n";
    let pack = python_pack();
    let ext = extract_file("inherit.py", src, &pack).unwrap();

    let inherits: Vec<_> = ext
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::Inherits)
        .collect();
    assert_eq!(inherits.len(), 1);
    assert!(inherits[0].source_id.contains("Child"));
    assert!(inherits[0].target_id.contains("Base"));
}

#[test]
fn test_extract_statements() {
    let src = b"def process(x):\n    if x > 0:\n        for i in range(x):\n            print(i)\n    else:\n        pass\n";
    let pack = python_pack();
    let ext = extract_file("stmts.py", src, &pack).unwrap();

    assert!(!ext.statements.is_empty(), "should extract statements");
    let kinds: Vec<&str> = ext.statements.iter().map(|s| s.kind.as_str()).collect();
    assert!(kinds.contains(&"If"), "expected If statement");
    assert!(kinds.contains(&"For"), "expected For statement");
    assert!(kinds.contains(&"Else"), "expected Else statement");
}

#[test]
fn test_extract_complexity() {
    let src = b"def complex_func(x, y):\n    if x > 0:\n        if y > 0:\n            return x + y\n        else:\n            return x\n    elif x < 0:\n        return y\n    else:\n        return 0\n";
    let pack = python_pack();
    let ext = extract_file("complex.py", src, &pack).unwrap();

    let func = ext
        .symbols
        .iter()
        .find(|s| s.name == "complex_func")
        .unwrap();
    assert!(
        func.complexity > 1,
        "complex function should have complexity > 1, got {}",
        func.complexity
    );
}

#[test]
fn test_extract_module_level_variables() {
    let src = b"MAX_SIZE = 100\nDEBUG = True\n\ndef work():\n    pass\n";
    let pack = python_pack();
    let ext = extract_file("config.py", src, &pack).unwrap();

    let vars: Vec<&str> = ext
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Variable)
        .map(|s| s.name.as_str())
        .collect();
    assert!(vars.contains(&"MAX_SIZE"));
    assert!(vars.contains(&"DEBUG"));
}

#[test]
fn test_extract_content_hash_deterministic() {
    let src = b"def foo():\n    pass\n";
    let pack = python_pack();
    let ext1 = extract_file("foo.py", src, &pack).unwrap();
    let ext2 = extract_file("foo.py", src, &pack).unwrap();
    assert_eq!(ext1.content_hash, ext2.content_hash);
}

#[test]
fn test_extract_content_hash_changes() {
    let pack = python_pack();
    let ext1 = extract_file("f.py", b"def a(): pass", &pack).unwrap();
    let ext2 = extract_file("f.py", b"def b(): pass", &pack).unwrap();
    assert_ne!(ext1.content_hash, ext2.content_hash);
}

#[test]
fn test_extract_symbol_ids_contain_file() {
    let src = b"def my_func():\n    pass\n\nclass MyClass:\n    def method(self):\n        pass\n";
    let pack = python_pack();
    let ext = extract_file("src/module.py", src, &pack).unwrap();

    for sym in &ext.symbols {
        assert!(
            sym.id.contains("src/module.py"),
            "symbol id should contain file path: {}",
            sym.id
        );
    }
}

#[test]
fn test_extract_span_line_numbers() {
    let src = b"def first():\n    pass\n\ndef second():\n    pass\n";
    let pack = python_pack();
    let ext = extract_file("lines.py", src, &pack).unwrap();

    let first = ext.symbols.iter().find(|s| s.name == "first").unwrap();
    assert_eq!(first.span.start_line, 1);

    let second = ext.symbols.iter().find(|s| s.name == "second").unwrap();
    assert!(second.span.start_line > first.span.start_line);
}

#[test]
fn test_extract_empty_file() {
    let pack = python_pack();
    let ext = extract_file("empty.py", b"", &pack).unwrap();
    assert!(ext.symbols.is_empty());
    assert!(ext.relations.is_empty());
    assert!(ext.statements.is_empty());
}

#[test]
fn test_extract_docstrings() {
    let src = b"class Greeter:\n    \"\"\"A friendly greeter.\"\"\"\n    def greet(self):\n        \"\"\"Say hello.\"\"\"\n        pass\n";
    let pack = python_pack();
    let ext = extract_file("doc.py", src, &pack).unwrap();

    let class = ext
        .symbols
        .iter()
        .find(|s| s.kind == SymbolKind::Class)
        .unwrap();
    assert!(class
        .docstring
        .as_deref()
        .unwrap_or("")
        .contains("friendly greeter"));

    let method = ext
        .symbols
        .iter()
        .find(|s| s.kind == SymbolKind::Method)
        .unwrap();
    assert!(method
        .docstring
        .as_deref()
        .unwrap_or("")
        .contains("Say hello"));
}

#[test]
fn test_extract_nested_class_method_not_top_level() {
    let src = b"class Outer:\n    class Inner:\n        def inner_method(self):\n            pass\n    def outer_method(self):\n        pass\n";
    let pack = python_pack();
    let ext = extract_file("nested.py", src, &pack).unwrap();

    let methods: Vec<&str> = ext
        .symbols
        .iter()
        .filter(|s| s.kind == SymbolKind::Method)
        .map(|s| s.name.as_str())
        .collect();
    assert!(methods.contains(&"outer_method"));
    assert!(methods.contains(&"inner_method"));
}

#[test]
fn test_extract_multiple_inheritance() {
    let src = b"class A:\n    pass\n\nclass B:\n    pass\n\nclass C(A, B):\n    pass\n";
    let pack = python_pack();
    let ext = extract_file("multi.py", src, &pack).unwrap();

    let inherits: Vec<_> = ext
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::Inherits)
        .collect();
    assert_eq!(inherits.len(), 2, "C inherits from both A and B");
}

#[test]
fn test_extract_receiver_on_method_call() {
    let src = b"def work():\n    self.save()\n    db.query('SELECT 1')\n";
    let pack = python_pack();
    let ext = extract_file("recv.py", src, &pack).unwrap();

    let calls_with_receiver: Vec<_> = ext
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::Calls && r.receiver.is_some())
        .collect();
    assert!(
        !calls_with_receiver.is_empty(),
        "method calls should have receiver"
    );
}

// C++ virtual-dispatch / pointer-and-reference-to-base call resolution
// (the ITpsContext/EntityTpsContext bug class): a call through a member
// field or a function parameter typed as a base/interface class must
// resolve `receiver` to the declared *type*, not the raw variable name —
// otherwise resolve_calls.rs's class_method_map lookup can never match.

#[test]
fn test_extract_receiver_resolves_member_field_to_declared_type() {
    let src = br#"
class IBase {
public:
    virtual int GetType() const = 0;
};

class Caller {
public:
    void CheckField() {
        int t = field->GetType();
    }
    IBase* field;
};
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_extension(".cpp").unwrap();
    let ext = extract_file("caller.cpp", src, pack).unwrap();

    let call = ext
        .relations
        .iter()
        .find(|r| r.kind == RelationKind::Calls && r.target_id.ends_with("::GetType"))
        .expect("GetType call site should be extracted");

    assert_eq!(
        call.receiver.as_deref(),
        Some("IBase"),
        "receiver for `field->GetType()` should resolve to the field's declared \
         type (IBase), not the raw variable name (field)"
    );
}

#[test]
fn test_extract_receiver_resolves_param_reference_to_declared_type() {
    let src = br#"
class IBase {
public:
    virtual int GetType() const = 0;
};

void UseContext(const IBase& ctx) {
    int t = ctx.GetType();
}
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_extension(".cpp").unwrap();
    let ext = extract_file("caller.cpp", src, pack).unwrap();

    let call = ext
        .relations
        .iter()
        .find(|r| r.kind == RelationKind::Calls && r.target_id.ends_with("::GetType"))
        .expect("GetType call site should be extracted");

    assert_eq!(
        call.receiver.as_deref(),
        Some("IBase"),
        "receiver for `ctx.GetType()` should resolve to the parameter's declared \
         type (IBase), not the raw parameter name (ctx)"
    );
}

#[test]
fn test_extract_receiver_resolves_local_var_pointer_to_declared_type() {
    // Mirrors the real HVT bug: `const zctField* ctField = Get(...); ctField->GetType();`
    // in Src/High/SS/Misc/zssSanitizer.cpp — ctField is a local variable, not a
    // member field or a parameter.
    let src = br#"
class IBase {
public:
    virtual int GetType() const = 0;
};

void SanitizeField() {
    const IBase* field = GetField();
    int t = field->GetType();
}
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_extension(".cpp").unwrap();
    let ext = extract_file("caller.cpp", src, pack).unwrap();

    let call = ext
        .relations
        .iter()
        .find(|r| r.kind == RelationKind::Calls && r.target_id.ends_with("::GetType"))
        .expect("GetType call site should be extracted");

    assert_eq!(
        call.receiver.as_deref(),
        Some("IBase"),
        "receiver for a local-variable call (`field->GetType()`) should resolve \
         to the variable's declared type (IBase), not the raw variable name (field)"
    );
}

#[test]
fn test_extract_receiver_resolves_local_var_declared_inside_else_clause() {
    // Mirrors the real HVT bug: Src/High/TKE/MetaData/FieldAttributesHandler.cpp
    // declares `const ITpsContext* tpsContext = model->GetTpsContext();` inside a
    // nested else-branch, then calls `tpsContext->GetEntity()` inside a further
    // nested else-branch. `else_clause` is a distinct node kind from
    // `compound_statement` in the grammar — a scan that only recurses into
    // `if_statement`/`compound_statement` silently skips every local variable
    // declared inside any `else { ... }` block.
    let src = br#"
class IBase {
public:
    int GetEntity() const;
};

void HandlePut(IBase* model) {
    if (model == nullptr) {
        LogError();
    } else {
        const IBase* tpsContext = model;
        if (tpsContext == nullptr) {
            LogError();
        } else {
            int entity = tpsContext->GetEntity();
        }
    }
}
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_extension(".cpp").unwrap();
    let ext = extract_file("caller.cpp", src, pack).unwrap();

    let call = ext
        .relations
        .iter()
        .find(|r| r.kind == RelationKind::Calls && r.target_id.ends_with("::GetEntity"))
        .expect("GetEntity call site should be extracted");

    assert_eq!(
        call.receiver.as_deref(),
        Some("IBase"),
        "receiver for a local variable declared inside an else-branch should \
         resolve to its declared type (IBase), not the raw variable name \
         (tpsContext) — else_clause must be recursed into, not skipped"
    );
}

#[test]
fn test_extract_method_symbol_id_is_class_scoped_for_cpp() {
    let src = br#"
class IBase {
public:
    virtual int GetType() const = 0;
};
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_file_with_content("ibase.h", src).unwrap();
    let ext = extract_file("ibase.h", src, pack).unwrap();

    let method = ext
        .symbols
        .iter()
        .find(|s| s.name == "GetType")
        .expect("GetType method should be extracted");

    assert_eq!(
        method.id, "ibase.h::IBase::GetType",
        "method symbol id must be class-scoped (file::Class::method), not just \
         file-scoped (file::method) — otherwise resolve_calls.rs's \
         class_method_map lookup can never match a resolved receiver type"
    );
}

#[test]
fn test_extract_method_symbol_id_is_class_scoped_for_java() {
    // Mirrors a real gap found in tto-engine-master: find_parent_class only
    // checked "class_definition" (Python's node kind), never "class_declaration"
    // (Java's actual node kind) — every Java method extracted file-scoped
    // ("BlobController.java::saveBlob") instead of class-scoped
    // ("BlobController.java::BlobController::saveBlob"), silently breaking
    // class_method_map-based receiver resolution and interface-sibling
    // lookups for every Java project, not just this one class.
    let src = br#"
public class BlobController {
    public void saveBlob() {
    }
}
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_extension(".java").unwrap();
    let ext = extract_file("BlobController.java", src, pack).unwrap();

    let method = ext
        .symbols
        .iter()
        .find(|s| s.name == "saveBlob")
        .expect("saveBlob method should be extracted");

    assert_eq!(
        method.id, "BlobController.java::BlobController::saveBlob",
        "Java method symbol id must be class-scoped (file::Class::method), \
         not file-scoped (file::method) — find_parent_class must recognize \
         Java's \"class_declaration\" node kind, not just Python's \
         \"class_definition\""
    );
}

#[test]
fn test_extract_pure_virtual_method_with_pointer_return_type() {
    // Mirrors the real HVT bug: Src/High/TKE/_h/ITpsContext.h declares
    // `virtual zccEntity* GetEntity() const = 0;` — a pure-virtual prototype
    // (no body) whose return type is a pointer. Every sibling pure-virtual
    // method with a non-pointer return type (bool, std::string, etc.)
    // extracted fine; only the pointer-returning one silently vanished,
    // because field_declaration's bodyless-prototype pattern had no wrapped
    // variant for a pointer_declarator between it and function_declarator.
    let src = br#"
class IBase {
public:
    virtual bool HasThing() const = 0;
    virtual Entity* GetEntity() const = 0;
};
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_extension(".cpp").unwrap();
    let ext = extract_file("ibase.cpp", src, pack).unwrap();

    assert!(
        ext.symbols.iter().any(|s| s.name == "GetEntity"),
        "pure-virtual method with a pointer return type must be extracted \
         as a symbol, same as any other pure-virtual method"
    );
}

#[test]
fn test_extract_module_scope_const_declaration_for_cpp() {
    // Real HVT gap: Src/High/SS/_h/efStaMgr.h declares
    // `const UINT16 kMaxEFEntities = 5;` at file scope, wrapped like nearly
    // every real C++ header in an #ifndef/#define/#endif include guard.
    // cpp's entities.scm had zero @var.def/@var.name captures at all, so
    // this constant — and every other module/namespace-scope const or
    // variable in any C++ file — was completely invisible to
    // search/trace_callers/query_graph, with no error, just silently absent
    // from the symbol list. The include-guard wrapping matters: an earlier,
    // unguarded version of this exact test passed even before the
    // preproc_ifdef fix below, masking the real bug — tree-sitter-cpp nests
    // guarded content under a preproc_ifdef node, not a direct child of
    // translation_unit, so the fix needed both a var.def pattern AND a
    // preproc_ifdef-wrapped variant of it to match real-world headers.
    let src = br#"
#ifndef __EFSTAMGR__
#define __EFSTAMGR__

class zefStateAssignmentTableAccessor;
const UINT16 kMaxEFEntities = 5; // maximum number of entities that is allowed

class zefStatusManager {
public:
    void Foo();
};

#endif
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_file_with_content("efStaMgr.h", src).unwrap();
    let ext = extract_file("efStaMgr.h", src, pack).unwrap();

    let konst = ext
        .symbols
        .iter()
        .find(|s| s.name == "kMaxEFEntities")
        .expect("kMaxEFEntities should be extracted as a Variable symbol");

    assert_eq!(konst.kind, SymbolKind::Variable);
    assert_eq!(konst.id, "efStaMgr.h::kMaxEFEntities");
}

#[test]
fn test_extract_guarded_function_prototype_for_cpp() {
    // Same preproc_ifdef gap, but for the pre-existing bodyless-prototype
    // function pattern (not just the new var.def pattern above): a plain
    // function prototype at file scope inside a real #ifndef include guard
    // was silently missing, same as every other translation_unit-scoped
    // pattern in this file, because tree-sitter-cpp nests guarded content
    // one level deeper than an unguarded file.
    let src = br#"
#ifndef __FOO_H__
#define __FOO_H__

void TopLevelPrototype(int x);

class Bar {
public:
    void Method1();
};

#endif
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_file_with_content("foo.h", src).unwrap();
    let ext = extract_file("foo.h", src, pack).unwrap();

    assert!(
        ext.symbols.iter().any(|s| s.name == "TopLevelPrototype"),
        "a bodyless function prototype inside a real #ifndef include guard \
         must be extracted as a symbol, same as an unguarded one"
    );
}

#[test]
fn test_extract_receiver_resolves_namespace_qualified_reference_type() {
    // Mirrors a real HVT bug found by cross-checking grep against the graph:
    // Src/High/SS/FormCalc/TKEMetaDataDependencies.cpp::GetCCFormInstances declares
    // `const tke::MappingMgr& mappingMgr = model->GetMappingMgr();` then calls
    // `mappingMgr.GetTpsInstanceValuesForInstanceXPath(...)`. The declared type is
    // namespace-qualified (tke::MappingMgr) — strip_type_qualifiers only stripped
    // const/volatile/*/&, so the resolved receiver stayed "tke::MappingMgr" and
    // never matched class_method_map's bare "MappingMgr" key.
    let src = br#"
namespace tke {
class MappingMgr {
public:
    int GetTpsInstanceValuesForInstanceXPath(int x);
};
}

void GetCCFormInstances() {
    const tke::MappingMgr& mappingMgr = GetMgr();
    int t = mappingMgr.GetTpsInstanceValuesForInstanceXPath(1);
}
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_extension(".cpp").unwrap();
    let ext = extract_file("caller.cpp", src, pack).unwrap();

    let call = ext
        .relations
        .iter()
        .find(|r| {
            r.kind == RelationKind::Calls
                && r.target_id
                    .ends_with("::GetTpsInstanceValuesForInstanceXPath")
        })
        .expect("GetTpsInstanceValuesForInstanceXPath call site should be extracted");

    assert_eq!(
        call.receiver.as_deref(),
        Some("MappingMgr"),
        "receiver for a namespace-qualified reference type (`const tke::MappingMgr&`) \
         should resolve to the bare class name (MappingMgr), not the fully-qualified \
         name (tke::MappingMgr) which never matches class_method_map's bare-name keys"
    );
}

#[test]
fn test_extract_receiver_captures_qualifier_on_static_qualified_call() {
    // Mirrors the real HVT bug: Test/Low/CT/ctTest.cpp:385 calls
    // `zctField::IsAcceptable(in, type, subType, &tainted)` — a static-qualified
    // call (Class::method(), no object/pointer receiver at all). The qualified
    // call pattern only captured @call.func (the method name), never the
    // qualifier, so the receiver was silently None and the call could only
    // ever resolve via a bare-name lookup — never via the (correct, unambiguous)
    // class name sitting right there in the qualifier.
    let src = br#"
class zctField {
public:
    static int IsAcceptable(const char* in);
};

int TestIsAcceptable(const char* in) {
    return zctField::IsAcceptable(in);
}
"#;
    let registry = infigraph_languages::bundled_registry().unwrap();
    let pack = registry.for_extension(".cpp").unwrap();
    let ext = extract_file("caller.cpp", src, pack).unwrap();

    let call = ext
        .relations
        .iter()
        .find(|r| r.kind == RelationKind::Calls && r.target_id.ends_with("::IsAcceptable"))
        .expect("IsAcceptable call site should be extracted");

    assert_eq!(
        call.receiver.as_deref(),
        Some("zctField"),
        "receiver for a static-qualified call (`zctField::IsAcceptable(...)`) \
         should capture the qualifier as the receiver, not leave it None"
    );
}

// ---------- Full pipeline: extract → graph → query ----------

#[test]
fn test_extract_to_graph_roundtrip() {
    use infigraph_core::graph::{GraphQuery, GraphStore};

    let src = b"class Service:\n    def handle(self, request):\n        result = self.validate(request)\n        return result\n\n    def validate(self, data):\n        if not data:\n            raise ValueError('empty')\n        return True\n\ndef test_handle():\n    svc = Service()\n    svc.handle({})\n";
    let pack = python_pack();
    let ext = extract_file("service.py", src, &pack).unwrap();

    let dir = tempfile::TempDir::new().unwrap();
    let store = GraphStore::open(&dir.path().join("graph")).unwrap();
    store.upsert_file(&ext).unwrap();

    let conn = store.connection().unwrap();
    let q = GraphQuery::new(&conn);

    let syms = q.symbols_in_file("service.py").unwrap();
    assert!(
        syms.len() >= 3,
        "expected Service, handle, validate, test_handle; got {}",
        syms.len()
    );

    let branches = q
        .branches_of(&syms.iter().find(|s| s.name == "validate").unwrap().id)
        .unwrap();
    assert!(
        !branches.is_empty(),
        "validate should have branches (if statement)"
    );
}
