(function_declaration name: (identifier) @name) @definition.function
(function_declaration name: (dot_index_expression field: (identifier) @name)) @definition.method
(function_declaration name: (method_index_expression method: (identifier) @name)) @definition.method
