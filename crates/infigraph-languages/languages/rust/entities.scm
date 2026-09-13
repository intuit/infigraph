; Rust entity extraction queries

; Function definitions
(function_item
  name: (identifier) @func.name) @func.def

; Struct definitions
(struct_item
  name: (type_identifier) @class.name) @class.def

; Enum definitions
(enum_item
  name: (type_identifier) @class.name) @class.def

; Trait definitions
(trait_item
  name: (type_identifier) @class.name) @class.def

; Impl block methods. impl_item has no "name" field (only body/trait/type/
; type_parameters, verified against tree-sitter-rust's node-types.json) --
; @method.parent captures the impl's own "type:" field (the Self type, e.g.
; `Bar` in `impl Foo for Bar`) so methods are scoped to it instead of falling
; back to a flat, unscoped file::method id.
(impl_item
  type: (_) @method.parent
  body: (declaration_list
    (function_item
      name: (identifier) @method.name) @method.def))

; Const items
(const_item
  name: (identifier) @var.name) @var.def

; Static items
(static_item
  name: (identifier) @var.name) @var.def

; Type aliases
(type_item
  name: (type_identifier) @class.name) @class.def
