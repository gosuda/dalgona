//! The definition extractor: recursive node-kind walks, one per language.
//!
//! One preorder pass builds the [`Def`] list in source order and counts
//! `ERROR`/`MISSING` nodes; the per-language classifiers fold grammar kinds
//! onto the closed [`Kind`] set.

use std::collections::HashMap;

use tree_sitter::{Node, Tree};

use super::{Def, Kind, Language};

/// A definition found at one node, before its qualified name is assembled.
struct Found<'t> {
    name: String,
    /// The segment path appended to the enclosing definition's qualified name.
    path: String,
    kind: Kind,
    span: Node<'t>,
}

impl<'t> Found<'t> {
    fn named(name: String, kind: Kind, span: Node<'t>) -> Self {
        Self {
            path: name.clone(),
            name,
            kind,
            span,
        }
    }
}

struct Walk<'s> {
    lang: Language,
    src: &'s [u8],
    defs: Vec<Def>,
    ordinals: HashMap<Box<str>, u32>,
}

impl Walk<'_> {
    /// Records the definition `node` forms, if any, and returns its index.
    ///
    /// `scope` is the qualifying container; `within` is the nearest enclosing
    /// definition of any kind, which is what folds methods. A trait is not a
    /// container, so its methods stay bare-qualified yet still fold to
    /// `method`.
    fn visit(
        &mut self,
        node: Node<'_>,
        scope: Option<usize>,
        within: Option<Kind>,
    ) -> Option<usize> {
        if !node.is_named() {
            return None;
        }
        let src = self.src;
        let found = match self.lang {
            Language::Rust => rust_def(node, src, within),
            Language::Ocaml | Language::OcamlInterface => ocaml_def(node, src),
            Language::Python => python_def(node, src, within),
            Language::JavaScript | Language::TypeScript | Language::Tsx => script_def(node, src),
            Language::Go => go_def(node, src),
            Language::C | Language::Cpp => c_family_def(node, src, within, self.lang),
        }?;
        let qualified: Box<str> = match scope {
            Some(index) => format!("{}::{}", self.defs[index].qualified, found.path).into(),
            None => found.path.into(),
        };
        let name: Box<str> = found.name.into();
        let count = self.ordinals.entry(name.clone()).or_insert(0);
        *count += 1;
        let (first, last) = lines(found.span);
        self.defs.push(Def {
            name,
            qualified,
            ordinal: *count,
            kind: found.kind,
            first,
            last,
            byte_start: found.span.start_byte(),
            byte_end: found.span.end_byte(),
        });
        Some(self.defs.len() - 1)
    }
}

/// One preorder pass: definitions in source order and the `ERROR`/`MISSING` count.
///
/// This is the recursive node-kind walk with its stack made explicit: the
/// `scopes` stack is the recursion's context parameter, and children are
/// visited in source order. A real recursion would risk a stack overflow on a
/// deeply nested tree, which aborts the process and cannot be caught at the
/// extractor boundary; the explicit stack keeps that input a normal answer.
pub(super) fn extract(lang: Language, tree: &Tree, src: &[u8]) -> (Vec<Def>, usize) {
    let mut walk = Walk {
        lang,
        src,
        defs: Vec::new(),
        ordinals: HashMap::new(),
    };
    let mut errors = 0;
    let mut cursor = tree.walk();
    let mut scopes: Vec<(Option<usize>, Option<Kind>)> = vec![(None, None)];
    loop {
        let node = cursor.node();
        let (scope, within) = scopes.last().copied().unwrap_or((None, None));
        if node.is_error() || node.is_missing() {
            errors += 1;
        }
        let inner = match walk.visit(node, scope, within) {
            Some(index) => (
                if is_container(walk.defs[index].kind, node) {
                    Some(index)
                } else {
                    scope
                },
                Some(walk.defs[index].kind),
            ),
            None => (scope, within),
        };
        if cursor.goto_first_child() {
            scopes.push(inner);
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return (walk.defs, errors);
            }
            scopes.pop();
        }
    }
}

/// Only containers give their qualified name to the definitions inside:
/// impls and classes always, modules and structs when they carry a body.
fn is_container(kind: Kind, node: Node<'_>) -> bool {
    match kind {
        Kind::Impl | Kind::Class => true,
        Kind::Module | Kind::Struct => node.child_by_field_name("body").is_some(),
        _ => false,
    }
}

/// 1-based first and last lines; a span ending at column 0 ends on the line before.
pub(super) fn lines(node: Node<'_>) -> (u32, u32) {
    let start = node.start_position();
    let end = node.end_position();
    let last = if end.column == 0 && end.row > start.row {
        end.row - 1
    } else {
        end.row
    };
    (line_number(start.row), line_number(last))
}

fn line_number(row: usize) -> u32 {
    u32::try_from(row).map_or(u32::MAX, |row| row.saturating_add(1))
}

/// The node's source text, lossily decoded.
pub(super) fn text(node: Node<'_>, src: &[u8]) -> String {
    String::from_utf8_lossy(src.get(node.byte_range()).unwrap_or_default()).into_owned()
}

fn field_text(node: Node<'_>, field: &str, src: &[u8]) -> Option<String> {
    node.child_by_field_name(field)
        .map(|child| text(child, src))
}

fn named_child_of<'t>(node: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .find(|child| kinds.contains(&child.kind()))
}

fn count_named(node: Node<'_>, kinds: &[&str]) -> usize {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| kinds.contains(&child.kind()))
        .count()
}

/// `parent` when it is a `wrapper` holding exactly one binding, so the span
/// covers the keyword; otherwise `node` itself.
fn sole<'t>(node: Node<'t>, wrapper: &str, bindings: &[&str]) -> Node<'t> {
    match node.parent() {
        Some(parent) if parent.kind() == wrapper && count_named(parent, bindings) == 1 => parent,
        _ => node,
    }
}

fn rust_def<'t>(node: Node<'t>, src: &[u8], within: Option<Kind>) -> Option<Found<'t>> {
    let kind = match node.kind() {
        "function_item" | "function_signature_item" => {
            if matches!(within, Some(Kind::Impl | Kind::Trait)) {
                Kind::Method
            } else {
                Kind::Function
            }
        }
        "struct_item" | "union_item" => Kind::Struct,
        "enum_item" => Kind::Enum,
        "trait_item" => Kind::Trait,
        "mod_item" => Kind::Module,
        "macro_definition" => Kind::Macro,
        "const_item" | "static_item" => Kind::Const,
        "type_item" | "associated_type" => Kind::Type,
        "impl_item" => {
            let name = rust_type_name(node.child_by_field_name("type")?, src)?;
            return Some(Found::named(name, Kind::Impl, node));
        }
        _ => return None,
    };
    Some(Found::named(field_text(node, "name", src)?, kind, node))
}

/// The bare type name of an impl target: `Vec` for `Vec<T>`, `Engine` for `crate::Engine`.
fn rust_type_name(mut node: Node<'_>, src: &[u8]) -> Option<String> {
    loop {
        node = match node.kind() {
            "generic_type" | "reference_type" | "pointer_type" => {
                node.child_by_field_name("type")?
            }
            "scoped_type_identifier" | "scoped_identifier" => node.child_by_field_name("name")?,
            _ => return Some(text(node, src)),
        };
    }
}

fn python_def<'t>(node: Node<'t>, src: &[u8], within: Option<Kind>) -> Option<Found<'t>> {
    let kind = match node.kind() {
        "function_definition" if within == Some(Kind::Class) => Kind::Method,
        "function_definition" => Kind::Function,
        "class_definition" => Kind::Class,
        _ => return None,
    };
    Some(Found::named(field_text(node, "name", src)?, kind, node))
}

fn script_def<'t>(node: Node<'t>, src: &[u8]) -> Option<Found<'t>> {
    let kind = match node.kind() {
        "function_declaration" | "generator_function_declaration" => Kind::Function,
        "class_declaration" | "abstract_class_declaration" | "class" => Kind::Class,
        "method_definition" => Kind::Method,
        "interface_declaration" => Kind::Interface,
        "enum_declaration" => Kind::Enum,
        "type_alias_declaration" => Kind::Type,
        "internal_module" | "module" => Kind::Module,
        "variable_declarator" => return script_const(node, src),
        _ => return None,
    };
    Some(Found::named(field_text(node, "name", src)?, kind, node))
}

/// A module-level `const` binding: a function when bound to a function value.
fn script_const<'t>(node: Node<'t>, src: &[u8]) -> Option<Found<'t>> {
    let declaration = node
        .parent()
        .filter(|parent| parent.kind() == "lexical_declaration")?;
    if declaration.child_by_field_name("kind")?.kind() != "const"
        || !matches!(declaration.parent()?.kind(), "program" | "export_statement")
    {
        return None;
    }
    let name = node
        .child_by_field_name("name")
        .filter(|name| name.kind() == "identifier")?;
    let kind = match node.child_by_field_name("value").map(|value| value.kind()) {
        Some("arrow_function" | "function_expression" | "function" | "generator_function") => {
            Kind::Function
        }
        _ => Kind::Const,
    };
    let span = if count_named(declaration, &["variable_declarator"]) == 1 {
        declaration
    } else {
        node
    };
    Some(Found::named(text(name, src), kind, span))
}

fn go_def<'t>(node: Node<'t>, src: &[u8]) -> Option<Found<'t>> {
    const TYPE_SPECS: &[&str] = &["type_spec", "type_alias"];
    match node.kind() {
        "function_declaration" => Some(Found::named(
            field_text(node, "name", src)?,
            Kind::Function,
            node,
        )),
        "method_declaration" => {
            let name = field_text(node, "name", src)?;
            let path = match go_receiver(node, src) {
                Some(receiver) => format!("{receiver}::{name}"),
                None => name.clone(),
            };
            Some(Found {
                name,
                path,
                kind: Kind::Method,
                span: node,
            })
        }
        "type_spec" | "type_alias" => {
            let kind = match node.child_by_field_name("type").map(|ty| ty.kind()) {
                Some("struct_type") if node.kind() == "type_spec" => Kind::Struct,
                Some("interface_type") if node.kind() == "type_spec" => Kind::Interface,
                _ => Kind::Type,
            };
            let span = sole(node, "type_declaration", TYPE_SPECS);
            Some(Found::named(field_text(node, "name", src)?, kind, span))
        }
        "const_spec" => {
            let declaration = node.parent()?;
            if declaration.parent()?.kind() != "source_file" {
                return None;
            }
            let span = sole(node, "const_declaration", &["const_spec"]);
            Some(Found::named(
                field_text(node, "name", src)?,
                Kind::Const,
                span,
            ))
        }
        _ => None,
    }
}

/// The receiver's type name of a Go method: `Engine` for `(e *Engine[T])`.
fn go_receiver(node: Node<'_>, src: &[u8]) -> Option<String> {
    let receiver = node.child_by_field_name("receiver")?;
    let parameter = named_child_of(receiver, &["parameter_declaration"])?;
    let mut ty = parameter.child_by_field_name("type")?;
    loop {
        ty = match ty.kind() {
            "pointer_type" => ty.named_child(0)?,
            "generic_type" => ty.child_by_field_name("type")?,
            _ => return Some(text(ty, src)),
        };
    }
}

fn c_family_def<'t>(
    node: Node<'t>,
    src: &[u8],
    within: Option<Kind>,
    lang: Language,
) -> Option<Found<'t>> {
    let cpp = lang == Language::Cpp;
    let has_body = || node.child_by_field_name("body").is_some();
    let kind = match node.kind() {
        "function_definition" => return c_function(node, src, within, lang),
        "type_definition" => {
            let name = declared_name(node.child_by_field_name("declarator")?)?;
            return Some(Found::named(text(name, src), Kind::Type, node));
        }
        "struct_specifier" | "union_specifier" if has_body() => Kind::Struct,
        "enum_specifier" if has_body() => Kind::Enum,
        "class_specifier" if cpp && has_body() => Kind::Class,
        "namespace_definition" if cpp => Kind::Module,
        "alias_declaration" if cpp => Kind::Type,
        "preproc_def" | "preproc_function_def" => Kind::Macro,
        _ => return None,
    };
    Some(Found::named(field_text(node, "name", src)?, kind, node))
}

fn c_function<'t>(
    node: Node<'t>,
    src: &[u8],
    within: Option<Kind>,
    lang: Language,
) -> Option<Found<'t>> {
    let cpp = lang == Language::Cpp;
    let target = declared_name(node.child_by_field_name("declarator")?)?;
    let path = text(target, src);
    let qualified = target.kind() == "qualified_identifier";
    let name = if qualified {
        let mut last = target;
        while last.kind() == "qualified_identifier" {
            last = last.child_by_field_name("name")?;
        }
        text(last, src)
    } else {
        path.clone()
    };
    let kind = if cpp && (qualified || matches!(within, Some(Kind::Class | Kind::Struct))) {
        Kind::Method
    } else {
        Kind::Function
    };
    Some(Found {
        name,
        path,
        kind,
        span: node,
    })
}

/// The name node under a C or C++ declarator chain.
fn declared_name(mut node: Node<'_>) -> Option<Node<'_>> {
    loop {
        node = match node.kind() {
            "identifier"
            | "field_identifier"
            | "type_identifier"
            | "qualified_identifier"
            | "destructor_name"
            | "operator_name"
            | "template_function" => return Some(node),
            "function_declarator"
            | "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "parenthesized_declarator"
            | "attributed_declarator" => node
                .child_by_field_name("declarator")
                .or_else(|| node.named_child(0))?,
            _ => return None,
        };
    }
}

fn ocaml_def<'t>(node: Node<'t>, src: &[u8]) -> Option<Found<'t>> {
    let child_text = |kinds: &[&str]| named_child_of(node, kinds).map(|child| text(child, src));
    match node.kind() {
        "let_binding" => ocaml_let(node, src),
        "type_binding" => Some(Found::named(
            field_text(node, "name", src)?,
            Kind::Type,
            sole(node, "type_definition", &["type_binding"]),
        )),
        "module_binding" => Some(Found::named(
            child_text(&["module_name"])?,
            Kind::Module,
            sole(node, "module_definition", &["module_binding"]),
        )),
        "module_type_definition" => Some(Found::named(
            child_text(&["module_type_name"])?,
            Kind::Interface,
            node,
        )),
        "exception_definition" => {
            let constructor = named_child_of(node, &["constructor_declaration"])?;
            let name = named_child_of(constructor, &["constructor_name"])?;
            Some(Found::named(text(name, src), Kind::Type, node))
        }
        "class_binding" => Some(Found::named(
            child_text(&["class_name"])?,
            Kind::Class,
            sole(node, "class_definition", &["class_binding"]),
        )),
        "method_definition" => Some(Found::named(
            child_text(&["method_name"])?,
            Kind::Method,
            node,
        )),
        "value_specification" | "external" => {
            let function = node.child_by_field_name("type").is_some_and(|ty| {
                ty.kind() == "function_type"
                    || (ty.kind() == "polymorphic_type"
                        && named_child_of(ty, &["function_type"]).is_some())
            });
            let kind = if function {
                Kind::Function
            } else {
                Kind::Const
            };
            Some(Found::named(child_text(&["value_name"])?, kind, node))
        }
        _ => None,
    }
}

/// A structure-level `let` binding of a plain name: a function when it takes
/// parameters or binds a function expression, else a constant.
fn ocaml_let<'t>(node: Node<'t>, src: &[u8]) -> Option<Found<'t>> {
    let definition = node
        .parent()
        .filter(|parent| parent.kind() == "value_definition")?;
    if !matches!(
        definition.parent()?.kind(),
        "compilation_unit" | "structure"
    ) {
        return None;
    }
    let name = node
        .child_by_field_name("pattern")
        .filter(|pattern| pattern.kind() == "value_name")?;
    let function = named_child_of(node, &["parameter"]).is_some()
        || matches!(
            node.child_by_field_name("body").map(|body| body.kind()),
            Some("fun_expression" | "function_expression")
        );
    let kind = if function {
        Kind::Function
    } else {
        Kind::Const
    };
    Some(Found::named(
        text(name, src),
        kind,
        sole(node, "value_definition", &["let_binding"]),
    ))
}
