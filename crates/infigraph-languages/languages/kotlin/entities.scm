; Kotlin entity extraction queries (tree-sitter-kotlin-ng grammar)

; Function declarations
; function_value_parameters and the return type are positional children
; on this grammar, not named fields -- extract_child_text can't reach
; either via child_by_field_name, so capture them explicitly here.
(function_declaration
  name: (identifier) @func.name
  (function_value_parameters) @func.params
  (type)? @func.return_type) @func.def

; Class declarations
(class_declaration
  name: (identifier) @class.name) @class.def

; Object declarations
(object_declaration
  name: (identifier) @class.name) @class.def

; === HTTP Route Patterns (Ktor) ===

; get("/path") { ... } / post("/path") { ... }
(call_expression
  (identifier) @route.method
  (value_arguments
    (value_argument
      (string_literal
        (string_content) @route.path)))
  (#match? @route.method "^(get|post|put|delete|patch|head|options|route|authenticate)$")) @route.def
