(class name: [(constant) (scope_resolution)] @name) @definition.class
(module name: [(constant) (scope_resolution)] @name) @definition.module
(program (method name: (_) @name) @definition.function)
(program (singleton_method name: (_) @name) @definition.function)
(body_statement (method name: (_) @name) @definition.method)
(body_statement (singleton_method name: (_) @name) @definition.method)
(assignment left: (constant) @name) @definition.constant
