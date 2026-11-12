(function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition.function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @definition.function
(function_definition declarator: (pointer_declarator declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name)))) @definition.function
(type_definition declarator: (type_identifier) @name) @definition.type
(struct_specifier name: (type_identifier) @name body: (_)) @definition.type
(union_specifier name: (type_identifier) @name body: (_)) @definition.type
(enum_specifier name: (type_identifier) @name body: (_)) @definition.enum
