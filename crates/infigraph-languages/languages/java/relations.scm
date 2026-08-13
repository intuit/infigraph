; Java relationship extraction queries

; Method invocations on objects: obj.method()
(method_invocation
  object: (_) @call.receiver
  name: (identifier) @call.func) @call.site

; Unqualified method invocations: method()
(method_invocation
  !object
  name: (identifier) @call.func) @call.site

; Object creation: new Foo()
(object_creation_expression
  type: (type_identifier) @call.func) @call.site

; Method references: Foo::bar, this::bar, obj::bar
; Not an invocation -- a reference to the method as a value (passed to
; Mono.deferContextual(Mono::just), .map(this::transform), Comparator.comparing(Foo::bar))
; -- but detect_dead_code only walks CALLS edges, so a method wired up only
; this way looked permanently dead. Same treatment as Kotlin's callable_reference
; fix. Grammar: method_reference := (type|primary_expression|super) '::' (identifier|'new').
; Anchor on the literal '::' token so only the identifier immediately after it
; (the method name) is captured -- never a receiver identifier (Foo in
; Foo::bar) and never Foo::new's ctor-reference, since 'new' is a keyword
; token, not an identifier node, so the pattern simply won't match there.
(method_reference
  "::"
  (identifier) @call.func .) @call.site

; Import declarations
(import_declaration
  (scoped_identifier) @import.module)

; Class inheritance: extends. May be type_identifier, generic_type, or
; scoped_type_identifier (e.g. class Foo extends Bar<T>, class Foo extends pkg.Bar).
(class_declaration
  name: (identifier) @inherit.child
  (superclass
    (_) @inherit.parent))

; Interface implementation: implements
(class_declaration
  name: (identifier) @inherit.child
  (super_interfaces
    (type_list
      (_) @inherit.parent)))
