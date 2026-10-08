pub(super) use super::tables::{Table, table};
use crate::parse::Language;
use std::fmt;
use tree_sitter::{Node, Tree};

/// Complexity and source-size measurements for one named function.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionMetrics {
    /// Function name from the syntax tree.
    pub name: Box<str>,
    /// First source line, one-based.
    pub start_line: u32,
    /// Last source line, one-based.
    pub end_line: u32,
    /// Sonar-style cognitive complexity.
    pub cognitive: u32,
    /// Cyclomatic complexity, with a baseline of one.
    pub cyclomatic: u32,
    /// Non-comment source lines containing code.
    pub ploc: u32,
    /// Maximum nested control-flow depth, excluding folded functions.
    pub nesting: u32,
}

/// Aggregate source-size and complexity measurements for one file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileMetrics {
    /// Non-comment source lines containing code in the file.
    pub ploc: u32,
    /// Named body-bearing functions in source order.
    pub functions: Vec<FunctionMetrics>,
    /// Sum of function cognitive complexity.
    pub cog_sum: u32,
    /// Sum of function cyclomatic complexity.
    pub cc_sum: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Metric {
    Cognitive,
    Cyclomatic,
    Ploc,
    Nesting,
}

impl fmt::Display for Metric {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cognitive => "cognitive",
            Self::Cyclomatic => "cyclomatic",
            Self::Ploc => "ploc",
            Self::Nesting => "nesting",
        })
    }
}

struct FunctionWork {
    name: Box<str>,
    start_line: u32,
    end_line: u32,
    cognitive: u32,
    cyclomatic: u32,
    cognitive_depth: u32,
    nesting_depth: u32,
    max_nesting: u32,
}

/// Converts a zero-based tree-sitter row or column into a one-based line number.
pub(super) fn one_based(value: usize) -> u32 {
    u32::try_from(value.saturating_add(1)).unwrap_or(u32::MAX)
}

impl FunctionWork {
    fn new(name: Box<str>, node: Node<'_>) -> Self {
        Self {
            name,
            start_line: one_based(node.start_position().row),
            end_line: one_based(node.end_position().row),
            cognitive: 0,
            cyclomatic: 1,
            cognitive_depth: 0,
            nesting_depth: 0,
            max_nesting: 0,
        }
    }

    fn finish(self, ploc: u32) -> FunctionMetrics {
        FunctionMetrics {
            name: self.name,
            start_line: self.start_line,
            end_line: self.end_line,
            cognitive: self.cognitive,
            cyclomatic: self.cyclomatic,
            ploc,
            nesting: self.max_nesting,
        }
    }
}

enum WalkEvent<'tree> {
    Enter(Node<'tree>),
    RestoreDepths {
        function_depth: usize,
        cognitive: u32,
        nesting: u32,
    },
    FinishFunction,
}

#[expect(
    clippy::too_many_lines,
    reason = "single tree walk whose arms share the work vector; splitting would thread six locals through helpers"
)]
pub(super) fn measure(language: Language, tree: &Tree, source: &[u8]) -> FileMetrics {
    let kinds = table(language);
    let mut comments = Vec::new();
    let mut work: Vec<FunctionWork> = Vec::new();
    let mut functions = Vec::new();
    let mut events = vec![WalkEvent::Enter(tree.root_node())];

    while let Some(event) = events.pop() {
        match event {
            WalkEvent::RestoreDepths {
                function_depth,
                cognitive,
                nesting,
            } => {
                if let Some(function) = work.get_mut(function_depth.saturating_sub(1)) {
                    function.cognitive_depth = cognitive;
                    function.nesting_depth = nesting;
                }
            }
            WalkEvent::FinishFunction => {
                if let Some(function) = work.pop() {
                    functions.push(function.finish(0));
                }
            }
            WalkEvent::Enter(node) => {
                let function_start = function_name(language, kinds, node, source)
                    .map(|name| {
                        work.push(FunctionWork::new(name, node));
                    })
                    .is_some();
                if kinds.comments.contains(&node.kind()) {
                    comments.push((node.start_byte(), node.end_byte()));
                }

                let mut depth_restore = None;
                if let Some(function) = work.last_mut() {
                    if kinds.decisions.contains(&node.kind()) {
                        function.cyclomatic = function.cyclomatic.saturating_add(1);
                    }
                    if node.kind() == kinds.bool_node {
                        let operator = operator_text(node, source);
                        if kinds.bool_ops.contains(&operator) {
                            function.cyclomatic = function.cyclomatic.saturating_add(1);
                            if node
                                .parent()
                                .is_none_or(|parent| parent.kind() != kinds.bool_node)
                            {
                                function.cognitive = function
                                    .cognitive
                                    .saturating_add(bool_chain_score(node, kinds, source));
                            }
                        }
                    }
                    if kinds.calls.contains(&node.kind())
                        && is_recursive_call(node, &function.name, source)
                    {
                        function.cognitive = function.cognitive.saturating_add(1);
                    }
                    if kinds.labeled_jumps.contains(&node.kind())
                        && node.child_by_field_name("label").is_some()
                    {
                        function.cognitive = function.cognitive.saturating_add(1);
                    }
                    if kinds.flat.contains(&node.kind()) {
                        function.cognitive = function.cognitive.saturating_add(1);
                    }

                    let else_if = is_else_if(kinds, node);
                    let ocaml_function_expression =
                        matches!(language, Language::Ocaml) && node.kind() == "function_expression";
                    let has_cognitive_nesting =
                        kinds.nesting.contains(&node.kind()) || ocaml_function_expression;
                    if has_cognitive_nesting && !else_if {
                        function.cognitive = function
                            .cognitive
                            .saturating_add(1_u32.saturating_add(function.cognitive_depth));
                        depth_restore = Some((function.cognitive_depth, function.nesting_depth));
                        function.cognitive_depth = function.cognitive_depth.saturating_add(1);
                        if !ocaml_function_expression {
                            function.nesting_depth = function.nesting_depth.saturating_add(1);
                            function.max_nesting = function.max_nesting.max(function.nesting_depth);
                        }
                    } else if else_if {
                        function.cognitive = function.cognitive.saturating_add(1);
                    } else if kinds.folded.contains(&node.kind()) {
                        depth_restore = Some((function.cognitive_depth, function.nesting_depth));
                        function.cognitive_depth = function.cognitive_depth.saturating_add(1);
                    }
                }

                if function_start {
                    events.push(WalkEvent::FinishFunction);
                }
                if let Some((cognitive, nesting)) = depth_restore {
                    events.push(WalkEvent::RestoreDepths {
                        function_depth: work.len(),
                        cognitive,
                        nesting,
                    });
                }
                for index in (0..node.child_count()).rev() {
                    if let Some(child) = node.child(index) {
                        events.push(WalkEvent::Enter(child));
                    }
                }
            }
        }
    }

    let prefix = code_line_prefix(source, &comments);
    for function in &mut functions {
        function.ploc = line_range(&prefix, function.start_line, function.end_line);
    }
    functions.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then_with(|| a.name.cmp(&b.name))
    });
    let cog_sum = functions.iter().fold(0_u32, |sum, function| {
        sum.saturating_add(function.cognitive)
    });
    let cc_sum = functions.iter().fold(0_u32, |sum, function| {
        sum.saturating_add(function.cyclomatic)
    });
    FileMetrics {
        ploc: prefix.last().copied().unwrap_or_default(),
        functions,
        cog_sum,
        cc_sum,
    }
}

pub(super) fn function_name(
    language: Language,
    kinds: &Table,
    node: Node<'_>,
    source: &[u8],
) -> Option<Box<str>> {
    if !kinds.functions.contains(&node.kind()) {
        return None;
    }
    let body_bearing = match language {
        Language::Ocaml => has_child_kind(node, "parameter"),
        Language::OcamlInterface => false,
        _ => node.child_by_field_name("body").is_some(),
    };
    if !body_bearing {
        return None;
    }
    let name = node_text(function_name_node(language, node)?, source).trim();
    (!name.is_empty()).then(|| name.into())
}

pub(super) fn function_name_node(language: Language, node: Node<'_>) -> Option<Node<'_>> {
    match language {
        Language::Ocaml => node.child_by_field_name("pattern"),
        Language::C | Language::Cpp => {
            node.child_by_field_name("declarator")
                .and_then(|declarator| {
                    first_named_node(
                        declarator,
                        &["identifier", "field_identifier", "qualified_identifier"],
                    )
                })
        }
        _ => node.child_by_field_name("name"),
    }
}

fn has_child_kind(node: Node<'_>, kind: &str) -> bool {
    (0..node.child_count())
        .filter_map(|index| node.child(index))
        .any(|child| child.is_named() && child.kind() == kind)
}

fn first_named_node<'tree>(node: Node<'tree>, kinds: &[&str]) -> Option<Node<'tree>> {
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if kinds.contains(&current.kind()) {
            return Some(current);
        }
        for index in (0..current.child_count()).rev() {
            if let Some(child) = current.child(index) {
                pending.push(child);
            }
        }
    }
    None
}

fn is_else_if(kinds: &Table, node: Node<'_>) -> bool {
    if !matches!(node.kind(), "if_expression" | "if_statement") {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    if !kinds.else_parents.contains(&parent.kind()) {
        return false;
    }
    if parent.kind() == "if_statement" {
        return parent
            .child_by_field_name("alternative")
            .is_some_and(|alternative| alternative.id() == node.id());
    }
    true
}

fn operator_text<'source>(node: Node<'_>, source: &'source [u8]) -> &'source str {
    node.child_by_field_name("operator")
        .or_else(|| node.child(1))
        .map_or("", |operator| node_text(operator, source))
}

enum BoolEvent<'tree, 'source> {
    Node(Node<'tree>),
    Operator(&'source str),
}

fn bool_chain_score(node: Node<'_>, kinds: &Table, source: &[u8]) -> u32 {
    let mut events = vec![BoolEvent::Node(node)];
    let mut previous = None;
    let mut score = 0_u32;
    while let Some(event) = events.pop() {
        match event {
            BoolEvent::Operator(operator) => {
                if previous != Some(operator) {
                    score = score.saturating_add(1);
                }
                previous = Some(operator);
            }
            BoolEvent::Node(current) => {
                if current.kind() != kinds.bool_node {
                    continue;
                }
                let operator = operator_text(current, source);
                if !kinds.bool_ops.contains(&operator) {
                    continue;
                }
                for index in (0..current.child_count()).rev() {
                    let Some(child) = current.child(index) else {
                        continue;
                    };
                    if child.kind() == kinds.bool_node {
                        events.push(BoolEvent::Node(child));
                    } else {
                        let operator = node_text(child, source);
                        if kinds.bool_ops.contains(&operator) {
                            events.push(BoolEvent::Operator(operator));
                        }
                    }
                }
            }
        }
    }
    score
}

fn is_recursive_call(node: Node<'_>, function_name: &str, source: &[u8]) -> bool {
    let callee = node
        .child_by_field_name("function")
        .or_else(|| node.child(0));
    let Some(callee) = callee else {
        return false;
    };
    node_text(callee, source)
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .rsplit([':', '.'])
        .next()
        .is_some_and(|name| name == function_name)
}

fn node_text<'source>(node: Node<'_>, source: &'source [u8]) -> &'source str {
    std::str::from_utf8(source.get(node.byte_range()).unwrap_or_default()).unwrap_or_default()
}

fn code_line_prefix(source: &[u8], comments: &[(usize, usize)]) -> Vec<u32> {
    #[expect(
        clippy::naive_bytecount,
        reason = "bytecount crate is not a dependency of this crate"
    )]
    let line_count = source
        .iter()
        .filter(|&&byte| byte == b'\n')
        .count()
        .saturating_add(1);
    let mut has_code = vec![false; line_count];
    let mut comment_index = 0;
    let mut line = 0_usize;
    for (offset, byte) in source.iter().copied().enumerate() {
        while comments
            .get(comment_index)
            .is_some_and(|(start, end)| *end <= offset || *start > offset)
        {
            if comments
                .get(comment_index)
                .is_some_and(|(_, end)| *end <= offset)
            {
                comment_index = comment_index.saturating_add(1);
            } else {
                break;
            }
        }
        let in_comment = comments
            .get(comment_index)
            .is_some_and(|(start, end)| *start <= offset && offset < *end);
        if byte == b'\n' {
            line = line.saturating_add(1);
        } else if !in_comment
            && !byte.is_ascii_whitespace()
            && let Some(code) = has_code.get_mut(line)
        {
            *code = true;
        }
    }
    let mut prefix: Vec<u32> = Vec::with_capacity(has_code.len().saturating_add(1));
    prefix.push(0);
    for contains_code in has_code {
        let next = prefix
            .last()
            .copied()
            .unwrap_or_default()
            .saturating_add(u32::from(contains_code));
        prefix.push(next);
    }
    prefix
}

fn line_range(prefix: &[u32], start_line: u32, end_line: u32) -> u32 {
    let Some(start) = start_line
        .checked_sub(1)
        .and_then(|line| usize::try_from(line).ok())
    else {
        return 0;
    };
    let Some(end) = usize::try_from(end_line).ok() else {
        return 0;
    };
    let last = prefix.len().saturating_sub(1);
    prefix
        .get(end.min(last))
        .copied()
        .unwrap_or_default()
        .saturating_sub(prefix.get(start.min(last)).copied().unwrap_or_default())
}

pub(super) fn mass(function: &FunctionMetrics) -> f64 {
    f64::from(function.cyclomatic) * f64::from(function.ploc).sqrt()
}

pub(super) fn erosion<'a>(functions: impl Iterator<Item = &'a FunctionMetrics>) -> f64 {
    let (eroded, total) = functions.fold((0.0, 0.0), |(eroded, total), function| {
        let function_mass = mass(function);
        let eroded = if function.cyclomatic > 10 {
            eroded + function_mass
        } else {
            eroded
        };
        (eroded, total + function_mass)
    });
    if total == 0.0 { 0.0 } else { eroded / total }
}

#[derive(Debug)]
pub(super) struct Crossing {
    pub line: String,
    pub delta_mass: f64,
}

/// Bands applied to metric checks; mirrors the core `[guard.bands]` table.
pub(super) struct Bands {
    /// Cognitive complexity band.
    pub cognitive: u32,
    /// Cyclomatic complexity band.
    pub cyclomatic: u32,
    /// Function physical-lines band.
    pub function_ploc: u32,
    /// Nesting depth band.
    pub nesting: u32,
    /// File physical-lines band.
    pub file_ploc: u32,
}

pub(super) fn crossings(
    path: &str,
    pre: Option<&FileMetrics>,
    post: &FileMetrics,
    bands: &Bands,
) -> Vec<Crossing> {
    let mut result = Vec::new();
    for function in &post.functions {
        let before = pre.and_then(|file| match_function(file, function));
        push_crossing(
            &mut result,
            function,
            before,
            Metric::Cognitive,
            function.cognitive,
            before.map(|f| f.cognitive),
            bands.cognitive,
        );
        push_crossing(
            &mut result,
            function,
            before,
            Metric::Cyclomatic,
            function.cyclomatic,
            before.map(|f| f.cyclomatic),
            bands.cyclomatic,
        );
        push_crossing(
            &mut result,
            function,
            before,
            Metric::Ploc,
            function.ploc,
            before.map(|f| f.ploc),
            bands.function_ploc,
        );
        push_crossing(
            &mut result,
            function,
            before,
            Metric::Nesting,
            function.nesting,
            before.map(|f| f.nesting),
            bands.nesting,
        );
    }
    let previous_ploc = pre.map(|file| file.ploc);
    if post.ploc > bands.file_ploc && previous_ploc.is_none_or(|before| post.ploc > before) {
        let before = previous_ploc.map_or(super::report::Before::New, super::report::Before::Value);
        result.push(Crossing {
            line: super::report::file_band_line(path, before, post.ploc, bands.file_ploc),
            delta_mass: 0.0,
        });
    }
    result
}

fn push_crossing(
    result: &mut Vec<Crossing>,
    function: &FunctionMetrics,
    before_function: Option<&FunctionMetrics>,
    metric: Metric,
    after: u32,
    before: Option<u32>,
    threshold: u32,
) {
    let before = match before {
        Some(value) if after > value && after > threshold => super::report::Before::Value(value),
        None if after > threshold => super::report::Before::New,
        _ => return,
    };
    let before_mass = before_function.map_or(0.0, mass);
    result.push(Crossing {
        line: super::report::band_line(&function.name, metric, before, after, threshold),
        delta_mass: mass(function) - before_mass,
    });
}

pub(super) fn match_function<'a>(
    pre: &'a FileMetrics,
    function: &FunctionMetrics,
) -> Option<&'a FunctionMetrics> {
    match_in(&pre.functions, function)
}

pub(super) fn match_in<'a>(
    functions: &'a [FunctionMetrics],
    function: &FunctionMetrics,
) -> Option<&'a FunctionMetrics> {
    let mut best: Option<&'a FunctionMetrics> = None;
    let mut best_distance = u32::MAX;
    let mut same_name = 0_usize;
    for candidate in functions {
        if candidate.name != function.name {
            continue;
        }
        same_name = same_name.saturating_add(1);
        let distance = candidate.start_line.abs_diff(function.start_line);
        if distance <= 5 && distance < best_distance {
            best_distance = distance;
            best = Some(candidate);
        }
    }
    best.or_else(|| {
        (same_name == 1)
            .then(|| {
                functions
                    .iter()
                    .find(|candidate| candidate.name == function.name)
            })
            .flatten()
    })
}
