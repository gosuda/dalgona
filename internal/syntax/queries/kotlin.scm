(class_declaration "interface" (type_identifier) @name) @definition.interface
(class_declaration (type_identifier) @name (enum_class_body)) @definition.enum
(class_declaration "class" (type_identifier) @name (class_body)) @definition.class
(object_declaration (type_identifier) @name) @definition.class
(source_file (function_declaration (simple_identifier) @name) @definition.function)
(class_body (function_declaration (simple_identifier) @name) @definition.method)
(enum_entry (simple_identifier) @name) @definition.constant
(type_alias (type_identifier) @name) @definition.type
