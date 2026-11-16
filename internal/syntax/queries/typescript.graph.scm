(call_expression function: (identifier) @call)
(call_expression function: (member_expression property: (property_identifier) @call))
(new_expression constructor: (identifier) @call)
(new_expression constructor: (member_expression property: (property_identifier) @call))
(type_identifier) @type
(extends_clause value: (identifier) @type)
(import_statement source: (string) @import)
(export_statement source: (string) @import)
