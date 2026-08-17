; C++ relationship extraction queries

; Function calls
(call_expression
  function: (identifier) @call.func) @call.site

; Method calls: obj.method() or obj->method()
(call_expression
  function: (field_expression
    argument: (_) @call.receiver
    field: (field_identifier) @call.func)) @call.site

; Qualified calls: ns::func() or Class::method() (static-qualified method call)
; scope captures the qualifier text as @call.receiver so resolve_with_map's
; receiver-aware Strategy 1 can match it directly against a real class name —
; without this, `zctField::IsAcceptable(...)` had no receiver at all, so the
; qualifier was silently discarded and the call could only ever resolve via
; a bare-name lookup, same failure mode as an unresolved variable receiver.
(call_expression
  function: (qualified_identifier
    scope: (_) @call.receiver
    name: (identifier) @call.func)) @call.site

; Template calls: func<T>() or ns::func<T>()
(call_expression
  function: (template_function
    name: (identifier) @call.func)) @call.site

(call_expression
  function: (qualified_identifier
    name: (template_function
      name: (identifier) @call.func))) @call.site

; Include directives
(preproc_include
  path: (_) @import.module)

; Base class specifier (inheritance): class Foo : public Bar
(class_specifier
  name: (type_identifier) @inherit.child
  (base_class_clause
    (type_identifier) @inherit.parent))

; Base class specifier (inheritance): struct Foo : public Bar
(struct_specifier
  name: (type_identifier) @inherit.child
  (base_class_clause
    (type_identifier) @inherit.parent))
