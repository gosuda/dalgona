(function_declaration (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(method_definition (property_identifier) @name) @definition.method
(method_definition (private_property_identifier) @name) @definition.method
(class_declaration (identifier) @name) @definition.class
(program (lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function))
(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function))
