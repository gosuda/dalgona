(function_declaration name: (identifier) @name) @definition.function
(method_declaration name: (field_identifier) @name) @definition.method
(method_elem name: (field_identifier) @name) @definition.method
(type_spec name: (type_identifier) @name type: (interface_type)) @definition.interface
(type_spec name: (type_identifier) @name type: [(struct_type) (type_identifier) (qualified_type) (generic_type) (pointer_type) (array_type) (slice_type) (map_type) (channel_type) (function_type) (parenthesized_type)]) @definition.type
(type_alias name: (type_identifier) @name) @definition.type
(const_spec . name: (identifier) @name) @definition.constant
(var_spec . name: (identifier) @name) @definition.variable
