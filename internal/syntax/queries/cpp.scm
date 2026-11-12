(function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition.function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @definition.function
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name))) @definition.method
(field_declaration_list (function_definition declarator: (function_declarator declarator: (field_identifier) @name)) @definition.method)
(type_definition declarator: (type_identifier) @name) @definition.type
(struct_specifier name: (type_identifier) @name body: (_)) @definition.type
(union_specifier name: (type_identifier) @name body: (_)) @definition.type
(class_specifier name: (type_identifier) @name body: (_)) @definition.class
(enum_specifier name: (type_identifier) @name body: (_)) @definition.enum
(namespace_definition name: (namespace_identifier) @name) @definition.module
