; TypeScript entity extraction queries

; Function declarations
(function_declaration
  name: (identifier) @func.name) @func.def

; Variable-bound callable definitions
(variable_declarator
  name: (identifier) @func.name
  value: [(arrow_function) (function_expression) (generator_function)]) @func.def

; Class declarations
(class_declaration
  name: (type_identifier) @class.name) @class.def

; Method definitions in classes
(method_definition
  name: (property_identifier) @method.name) @method.def

; Interface declarations
(interface_declaration
  name: (type_identifier) @class.name) @class.def

; Enum declarations
(enum_declaration
  name: (identifier) @class.name) @class.def

; Type alias declarations
(type_alias_declaration
  name: (type_identifier) @class.name) @class.def

; Variable/const declarations (non-arrow)
(variable_declarator
  name: (identifier) @var.name) @var.def

; === HTTP Route Patterns (Express, Koa, Hono, Fastify, NestJS) ===

; router.get("/path", handler) — with named handler
(expression_statement
  (call_expression
    function: (member_expression
      property: (property_identifier) @route.method)
    arguments: (arguments
      (string) @route.path
      (identifier) @route.handler)) @route.def
  (#match? @route.method "^(get|post|put|delete|patch|options|head|all|use)$"))

; router.get("/path", ...) — anonymous handler
(expression_statement
  (call_expression
    function: (member_expression
      property: (property_identifier) @route.method)
    arguments: (arguments
      (string) @route.path)) @route.def
  (#match? @route.method "^(get|post|put|delete|patch|options|head|all|use)$"))

; Template literal paths
(expression_statement
  (call_expression
    function: (member_expression
      property: (property_identifier) @route.method)
    arguments: (arguments
      (template_string) @route.path)) @route.def
  (#match? @route.method "^(get|post|put|delete|patch|options|head|all|use)$"))

; === Next.js App Router ===
(export_statement
  (function_declaration
    name: (identifier) @route.method) @route.def
  (#match? @route.method "^(GET|POST|PUT|DELETE|PATCH|HEAD|OPTIONS)$"))
