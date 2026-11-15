(call_expression function: (identifier) @call)
(call_expression function: (field_expression field: (field_identifier) @call))
(call_expression function: (scoped_identifier name: (identifier) @call))
(call_expression function: (generic_function function: (identifier) @call))
(call_expression function: (generic_function function: (scoped_identifier name: (identifier) @call)))
(call_expression function: (generic_function function: (field_expression field: (field_identifier) @call)))
(type_identifier) @type
(use_declaration) @import
(extern_crate_declaration name: (identifier) @import)
