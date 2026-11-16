(protocol_declaration (type_identifier) @name) @definition.interface
(class_declaration "class" (type_identifier) @name (class_body)) @definition.class
(class_declaration "actor" (type_identifier) @name (class_body)) @definition.class
(class_declaration "struct" (type_identifier) @name (class_body)) @definition.type
(class_declaration (type_identifier) @name (enum_class_body)) @definition.enum
(protocol_function_declaration (simple_identifier) @name) @definition.method
(class_body (function_declaration (simple_identifier) @name) @definition.method)
(source_file (function_declaration (simple_identifier) @name) @definition.function)
(enum_entry (simple_identifier) @name) @definition.constant
(typealias_declaration name: (type_identifier) @name) @definition.type
