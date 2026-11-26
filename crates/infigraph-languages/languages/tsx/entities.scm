; TSX entity extraction queries (reuses TypeScript patterns)

; Function declarations
(function_declaration
  name: (identifier) @func.name) @func.def

; Variable-bound callables
(variable_declarator
  name: (identifier) @func.name
  value: [(arrow_function) (function_expression) (generator_function)]) @func.def

; Class declarations
(class_declaration
  name: (type_identifier) @class.name) @class.def

; Interface declarations
(interface_declaration
  name: (type_identifier) @class.name) @class.def

; Method definitions
(method_definition
  name: (property_identifier) @method.name) @method.def

; Variable declarations (const/let/var with arrow functions or values)
(lexical_declaration
  (variable_declarator
    name: (identifier) @var.name)) @var.def

; Type alias declarations
(type_alias_declaration
  name: (type_identifier) @class.name) @class.def

; Enum declarations
(enum_declaration
  name: (identifier) @class.name) @class.def

; === HTTP Route Patterns ===
(expression_statement
  (call_expression
    function: (member_expression
      property: (property_identifier) @route.method)
    arguments: (arguments
      (string) @route.path)) @route.def
  (#match? @route.method "^(get|post|put|delete|patch|options|head|all|use)$"))
