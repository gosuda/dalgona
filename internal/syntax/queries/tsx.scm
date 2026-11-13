(function_declaration (identifier) @name) @definition.function
(generator_function_declaration name: (identifier) @name) @definition.function
(method_definition (property_identifier) @name) @definition.method
(method_definition (private_property_identifier) @name) @definition.method
(class_declaration (type_identifier) @name) @definition.class
(abstract_class_declaration name: (type_identifier) @name) @definition.class
(interface_declaration (type_identifier) @name) @definition.interface
(enum_declaration (identifier) @name) @definition.enum
(type_alias_declaration (type_identifier) @name) @definition.type
(internal_module name: (identifier) @name) @definition.module
(module name: (identifier) @name) @definition.module
(program (lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function))
(export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @definition.function))
