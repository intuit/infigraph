; Dart entity extraction queries

; Class declarations
(class_declaration
  name: (identifier) @class.name) @class.def

; Enum declarations
(enum_declaration
  name: (identifier) @class.name) @class.def

; Mixin declarations
(mixin_declaration
  (identifier) @class.name) @class.def

; Extension declarations
(extension_declaration
  name: (identifier) @class.name) @class.def

; Top-level function declarations
; function_signature genuinely has "parameters"/"return_type" named
; fields, but they're one level below @func.def (reached via
; function_declaration's own "signature" field) -- capture them
; directly here rather than relying on child_by_field_name on the
; outer node, which can't see through that extra level.
; Field order below must match the grammar's actual child order
; (return_type, name, parameters) -- tree-sitter's query matcher is
; order-sensitive across sibling field patterns even when each is
; field-qualified, so declaring them out of order silently fails to
; match the return_type capture.
(function_declaration
  signature: (function_signature
    return_type: (type)? @func.return_type
    name: (identifier) @func.name
    parameters: (formal_parameter_list) @func.params)) @func.def

; Method signatures
; method_signature has ZERO named fields of its own -- there is no
; child_by_field_name path to "parameters" here at any nesting depth,
; so this capture is required, not just an optimization.
(method_signature
  (function_signature
    return_type: (type)? @method.return_type
    name: (identifier) @method.name
    parameters: (formal_parameter_list) @method.params)) @method.def

; Constructor signatures
(constructor_signature
  name: (identifier) @method.name) @method.def

; Top-level variables
(top_level_variable_declaration
  (static_final_declaration_list
    (static_final_declaration
      name: (identifier) @var.name))) @var.def

; === HTTP Route Patterns (shelf, dart_frog) ===
(expression_statement
  (call_expression
    function: (identifier) @route.method
    arguments: (arguments
      (string_literal) @route.path))
  (#match? @route.method "^(get|post|put|delete|patch|mount)$")) @route.def
