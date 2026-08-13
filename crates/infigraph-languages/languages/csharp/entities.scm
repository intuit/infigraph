; C# entity extraction queries

; Class declarations
(class_declaration
  name: (identifier) @class.name) @class.def

; Interface declarations
(interface_declaration
  name: (identifier) @class.name) @class.def

; Struct declarations
(struct_declaration
  name: (identifier) @class.name) @class.def

; Enum declarations
(enum_declaration
  name: (identifier) @class.name) @class.def

; Method declarations
(method_declaration
  name: (identifier) @method.name) @method.def

; Constructor declarations — previously uncaptured, so every call made
; inside a constructor body (DI/init wiring, e.g. `myMainViewModel =
; new MainViewModel(); myMainViewModel.Initialize(...);`) had no enclosing
; symbol to attach the CALLS relation to. Named after their class in C#, so
; @method.name captures the class name itself, which is also correct for
; call-site attribution — a call inside `MainWin() { ... }` should be
; attributed to `MainWin::MainWin`, matching this file's class-scoped id
; convention.
(constructor_declaration
  name: (identifier) @method.name) @method.def

; Namespace declarations
(namespace_declaration
  name: (identifier) @class.name) @class.def

; Property declarations
(property_declaration
  name: (identifier) @var.name) @var.def

; Field declarations (`private Bar myBar;`) — previously uncaptured entirely,
; so calls made through a plain field (as opposed to an auto-property) never
; had a symbol to resolve the receiver's declared type against, e.g.
; `myBar.Initialize()` left receiver as the raw field name "myBar" with
; nothing in class_method_map to match it to Bar::Initialize.
(field_declaration
  (variable_declaration
    (variable_declarator
      name: (identifier) @var.name))) @var.def

; === HTTP Route Patterns (ASP.NET Minimal APIs) ===

; app.MapGet("/path", handler) / app.MapPost / app.MapPut / app.MapDelete
(expression_statement
  (invocation_expression
    function: (member_access_expression
      name: (identifier) @route.method)
    arguments: (argument_list
      (argument
        (string_literal) @route.path))) @route.def
  (#match? @route.method "^(MapGet|MapPost|MapPut|MapDelete|MapPatch|MapGroup|Map)$"))
