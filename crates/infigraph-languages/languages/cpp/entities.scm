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

(field_declaration
  declarator: (function_declarator
    declarator: (field_identifier) @method.name)) @method.def

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
