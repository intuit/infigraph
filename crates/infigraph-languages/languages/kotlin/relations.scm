; Kotlin relationship extraction queries

; Simple function calls: funcName()
(call_expression
  (identifier) @call.func) @call.site

; Method calls: obj.method() / this.method() / super.method()
; `super_expression` (unlike C#'s anonymous `base` token) is a named node in
; this grammar, so the generic `(_)` wildcard here already matches it — no
; change needed for that case. receiver_text then comes through as the
; literal text "super", which relations.rs's self/this receiver-to-class
; normalization deliberately does NOT special-case: resolving it to the
; *subclass* would make Strategy 1 match the override itself (self-edge,
; wrong), and there's no cheap way from this per-file query to name the
; actual superclass. Leaving it unnormalized means it correctly falls
; through to the external_calls / EXTERNAL_CALL tracking path instead of
; being silently dropped or mis-resolved.
(call_expression
  (navigation_expression
    (_) @call.receiver
    (identifier) @call.func)) @call.site

; Bare / type-qualified method references: ::foo, Foo::bar
; Not an invocation — a reference to the method as a value (passed to
; .map(::foo), .onErrorMap(::foo), Comparator.comparing(Foo::bar), etc) —
; but detect_dead_code only walks CALLS edges, so a method wired up only
; this way looked permanently dead. Same treatment as C#'s delegate-ctor
; method-group capture: tag the referenced name as @call.func so it counts
; as reachable; unresolvable targets are dropped downstream like any other
; unresolved call.
(callable_reference
  (identifier) @call.func) @call.site

; Import declarations
(import
  (identifier) @import.module)

; Class inheritance / interface implementation: class Dog : Animal() or class Foo : Bar
; (Animal/Bar may be generic e.g. Comparable<Dog>, or qualified e.g. pkg.Animal)
(class_declaration
  name: (identifier) @inherit.child
  (delegation_specifiers
    (delegation_specifier
      [
        (user_type) @inherit.parent
        (constructor_invocation (user_type) @inherit.parent)
      ])))
