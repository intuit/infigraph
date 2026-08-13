; C# relationship extraction queries

; Method invocations: obj.Method()
(invocation_expression
  function: (member_access_expression
    expression: (_) @call.receiver
    name: (identifier) @call.func)) @call.site

; Simple invocations
(invocation_expression
  function: (identifier) @call.func) @call.site

; Object creation: new Foo()
(object_creation_expression
  type: (identifier) @call.func) @call.site

; Delegate construction with a method-group argument: new RoutedEventHandler(Foo)
; / new WaitCallback(this.Foo). This isn't an invocation — it's a reference to
; the method as a value — but detect_dead_code's reachability analysis only
; walks CALLS edges, so a method only ever wired up this way (event handlers,
; ThreadPool/Timer callbacks, PropertyChangedCallback) was flagged dead despite
; being reachable at runtime. Capture the bare-identifier/member-access argument
; itself as the call target; unresolvable candidates (a real variable passed to
; a real constructor) are dropped downstream same as any other unresolved
; @call.func, so this doesn't need to distinguish delegate types from ordinary
; constructors at extraction time.
(object_creation_expression
  arguments: (argument_list
    (argument
      (identifier) @call.func))) @call.site

(object_creation_expression
  arguments: (argument_list
    (argument
      (member_access_expression
        name: (identifier) @call.func)))) @call.site

; Using directives
(using_directive
  (identifier) @import.module)

; Using directives (qualified name)
(using_directive
  (qualified_name) @import.module)

; Class inheritance: base list
(class_declaration
  name: (identifier) @inherit.child
  (base_list
    (identifier) @inherit.parent))

; Interface inheritance
(interface_declaration
  name: (identifier) @inherit.child
  (base_list
    (identifier) @inherit.parent))
