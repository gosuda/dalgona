(source_file (function_item name: (identifier) @name) @definition.function)
(mod_item body: (declaration_list (function_item name: (identifier) @name) @definition.function))
(impl_item body: (declaration_list (function_item name: (identifier) @name) @definition.method))
(trait_item body: (declaration_list (function_signature_item name: (identifier) @name) @definition.method))
(trait_item body: (declaration_list (function_item name: (identifier) @name) @definition.method))
(struct_item name: (type_identifier) @name) @definition.type
(union_item name: (type_identifier) @name) @definition.type
(type_item name: (type_identifier) @name) @definition.type
(enum_item name: (type_identifier) @name) @definition.enum
(trait_item name: (type_identifier) @name) @definition.interface
(mod_item name: (identifier) @name) @definition.module
(const_item name: (identifier) @name) @definition.constant
(static_item name: (identifier) @name) @definition.variable
