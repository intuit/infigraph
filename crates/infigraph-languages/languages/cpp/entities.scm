; C++ entity extraction queries

; Function definitions.
; Anchored to function_definition (a declarator with a body) rather than to a
; bare function_declarator: `Type name(args);` is grammatically ambiguous with
; a function declaration ("most vexing parse"), so an unanchored pattern turns
; every parenthesised local variable into a phantom Function symbol, which then
; pollutes the cross-file resolver's candidate sets.
; The declarator may be wrapped (pointer/reference) before the
; function_declarator, so match it at any depth under the definition.
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @func.name)) @func.def

(function_definition
  declarator: (_
    (function_declarator
      declarator: (identifier) @func.name))) @func.def

; Method definitions: in-class bodies, and out-of-line `Class::method` bodies.
; Each shape needs a wrapped variant too: a pointer/reference return type
; (`const char* Class::method()`) puts a pointer_declarator between the
; function_definition and the function_declarator, so the unwrapped pattern
; alone silently misses every pointer-returning method.
(function_definition
  declarator: (function_declarator
    declarator: (field_identifier) @method.name)) @method.def

(function_definition
  declarator: (_
    (function_declarator
      declarator: (field_identifier) @method.name))) @method.def

(function_definition
  declarator: (function_declarator
    declarator: (qualified_identifier
      name: (identifier) @method.name))) @method.def

(function_definition
  declarator: (_
    (function_declarator
      declarator: (qualified_identifier
        name: (identifier) @method.name)))) @method.def

; Bodyless prototypes still matter as call targets. Only match them at
; translation-unit, namespace, or class scope — a `declaration` *inside* a
; function body is grammatically identical to a parenthesised local variable
; (`Serializer s(policy);`), so matching those anywhere would reintroduce the
; phantom-symbol pollution the anchoring above removes.
(translation_unit
  (declaration
    declarator: (function_declarator
      declarator: (identifier) @func.name)) @func.def)

(namespace_definition
  body: (declaration_list
    (declaration
      declarator: (function_declarator
        declarator: (identifier) @func.name)) @func.def))

; Module/namespace-scope variable and const declarations
; (`const UINT16 kMaxEFEntities = 5;`). Anchored the same way as the
; bodyless-prototype function patterns above (translation-unit/namespace
; scope only) for the same reason: a bare `declaration` inside a function
; body is grammatically identical to a parenthesised local variable, so
; matching those anywhere would create phantom Variable symbols for every
; local. `const zctField* ctField = Get(...);`-style locals are deliberately
; NOT captured here — only true file/namespace-scope declarations are.
; declarator may be a bare identifier or an init_declarator (initialized
; declaration) wrapping one — same shape find_local_var_type already
; handles in relations.rs for the resolver side of this.
(translation_unit
  (declaration
    declarator: (identifier) @var.name) @var.def)

(translation_unit
  (declaration
    declarator: (init_declarator
      declarator: (identifier) @var.name)) @var.def)

(namespace_definition
  body: (declaration_list
    (declaration
      declarator: (identifier) @var.name) @var.def))

(namespace_definition
  body: (declaration_list
    (declaration
      declarator: (init_declarator
        declarator: (identifier) @var.name)) @var.def))

; Nearly every real C++ header wraps its entire body in an include guard
; (#ifndef FOO_H / #define FOO_H / ... / #endif). tree-sitter-cpp nests
; that guarded content under a preproc_ifdef node instead of leaving it a
; direct child of translation_unit, so every translation_unit-scoped
; pattern above (and the bodyless-prototype function patterns futher up
; this file) silently misses anything inside a real, guarded header —
; which in practice is almost all of them. Re-declare the same
; declaration/declarator shapes one level down inside preproc_ifdef.
(preproc_ifdef
  (declaration
    declarator: (identifier) @var.name) @var.def)

(preproc_ifdef
  (declaration
    declarator: (init_declarator
      declarator: (identifier) @var.name)) @var.def)

(preproc_ifdef
  (declaration
    declarator: (function_declarator
      declarator: (identifier) @func.name)) @func.def)

(field_declaration
  declarator: (function_declarator
    declarator: (field_identifier) @method.name)) @method.def

; Bodyless prototypes (pure-virtual `= 0`, plain declarations) with a
; pointer/reference return type (`virtual zccEntity* GetEntity() const = 0;`)
; put a pointer_declarator/reference_declarator between field_declaration and
; function_declarator — same wrapping issue the function_definition patterns
; above already handle, but this field_declaration pattern was never given
; the wrapped equivalent, so every pointer/reference-returning pure-virtual
; method silently failed to extract as a symbol at all.
(field_declaration
  declarator: (_
    (function_declarator
      declarator: (field_identifier) @method.name))) @method.def

; Class definitions
(class_specifier
  name: (type_identifier) @class.name) @class.def

; Struct definitions
(struct_specifier
  name: (type_identifier) @class.name
  body: (_)) @class.def

; Union definitions
(union_specifier
  name: (type_identifier) @class.name
  body: (_)) @class.def

; Enum definitions
(enum_specifier
  name: (type_identifier) @class.name) @class.def

; Typedef declarations
(type_definition
  declarator: (type_identifier) @class.name) @class.def

; Namespace definitions
(namespace_definition
  name: (namespace_identifier) @class.name) @class.def
