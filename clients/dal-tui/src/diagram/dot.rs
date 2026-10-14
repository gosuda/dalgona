//! DOT validation plus a minimal cell raster over parsed node names.

use super::{ArtCell, ArtRole, DiagramArt, DiagramOutcome};

/// Renders DOT source or returns `invalid dot` for rejected syntax.
#[must_use]
pub fn render(source: &[u8], cap: usize) -> DiagramOutcome {
    let Ok(text) = std::str::from_utf8(source) else {
        return fallback("invalid dot");
    };
    if reject(text) {
        return fallback("invalid dot");
    }
    let names = node_names(text);
    if names.is_empty() {
        return fallback("invalid dot");
    }
    let line = names.join(" -> ");
    if line.chars().count() > cap.max(1) {
        return fallback("too wide");
    }
    DiagramOutcome::Art(DiagramArt {
        width_cells: line.chars().count(),
        rows: vec![line.chars().map(cell_for).collect()],
    })
}

fn reject(text: &str) -> bool {
    if text.contains('<') || text.contains("&lt;") {
        return true;
    }
    if text.contains("--") && text.contains("->") {
        return true;
    }
    if graph_keyword_count(text) > 1 {
        return true;
    }
    header_body(text).is_none()
}

fn graph_keyword_count(text: &str) -> usize {
    text.split(|value: char| !is_word_char(value))
        .filter(|word| *word == "graph" || *word == "digraph")
        .count()
}

fn header_body(text: &str) -> Option<&str> {
    let open = text.find('{')?;
    let close = text.rfind('}')?;
    if close <= open {
        return None;
    }
    let prefix = text.get(..open)?;
    if !valid_header(prefix) {
        return None;
    }
    let suffix = text.get(close + 1..)?;
    if !suffix.trim().is_empty() {
        return None;
    }
    text.get(open + 1..close)
}

fn valid_header(prefix: &str) -> bool {
    let text = prefix.trim();
    let after_strict = strip_word(text, "strict").map_or(text, |rest| rest.trim_start());
    let Some(after_keyword) =
        strip_word(after_strict, "digraph").or_else(|| strip_word(after_strict, "graph"))
    else {
        return false;
    };
    let id = after_keyword.trim_start();
    if id.is_empty() {
        return true;
    }
    if id.starts_with('"') {
        return quoted_id_rest_empty(id);
    }
    if id.chars().any(char::is_whitespace) {
        return false;
    }
    id.chars().all(is_word_char)
}

fn quoted_id_rest_empty(id: &str) -> bool {
    let Some(inner) = id.get(1..) else {
        return false;
    };
    let Some(end) = inner.find('"') else {
        return false;
    };
    let Some(rest) = inner.get(end + 1..) else {
        return false;
    };
    rest.trim().is_empty()
}

fn strip_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(word)?;
    if rest.is_empty() {
        return Some(rest);
    }
    let next = rest.chars().next()?;
    if is_word_char(next) {
        return None;
    }
    Some(rest)
}

fn is_word_char(value: char) -> bool {
    value.is_ascii_alphanumeric() || value == '_'
}

fn node_names(text: &str) -> Vec<String> {
    let Some(body) = header_body(text) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for statement in body.split([';', '\n']) {
        take_names(statement, &mut names);
    }
    names
}

fn take_names(fragment: &str, names: &mut Vec<String>) {
    let fragment = fragment.trim().trim_matches(['{', '}']).trim();
    if fragment.is_empty() {
        return;
    }
    if fragment.contains("->") {
        for part in fragment.split("->") {
            push_name(part.trim(), names);
        }
        return;
    }
    if fragment.contains("--") {
        for part in fragment.split("--") {
            push_name(part.trim(), names);
        }
    }
}

fn push_name(token: &str, names: &mut Vec<String>) {
    let token = token.trim().trim_matches('"').trim();
    if token.is_empty() || token.contains(' ') || token.contains('[') {
        return;
    }
    if !names.iter().any(|name| name == token) {
        names.push(token.to_owned());
    }
}

fn cell_for(character: char) -> ArtCell {
    let scalar = character as u32;
    let role = if matches!(character, '-' | '>' | '|') {
        ArtRole::Edge
    } else if (0x2500..=0x257F).contains(&scalar) {
        ArtRole::Border
    } else {
        ArtRole::Text
    };
    ArtCell {
        text: character.to_string(),
        role,
    }
}

fn fallback(reason: &str) -> DiagramOutcome {
    DiagramOutcome::Fallback {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::diagram::{ArtRole, DiagramOutcome};

    fn art_text(art: &crate::diagram::DiagramArt) -> String {
        art.rows
            .iter()
            .flatten()
            .map(|cell| cell.text.as_str())
            .collect()
    }

    #[test]
    fn validator_rejects_nested_and_html_labels() {
        assert!(matches!(
            render(b"digraph { graph { A } }", 80),
            DiagramOutcome::Fallback { .. }
        ));
        assert!(matches!(
            render(b"digraph { A [label=<b>x</b>] }", 80),
            DiagramOutcome::Fallback { .. }
        ));
    }

    #[test]
    fn simple_graph_renders_art_with_text_roles() {
        let DiagramOutcome::Art(art) = render(b"graph {\nA -> B\n}", 80) else {
            panic!("simple graph must render Art, not Fallback");
        };
        assert!(art.width_cells <= 80);
        assert!(
            art.rows
                .iter()
                .flatten()
                .any(|cell| cell.role == ArtRole::Text),
            "node names keep their Text role"
        );
    }

    #[test]
    fn multiline_digraph_renders_both_nodes() {
        let DiagramOutcome::Art(art) = render(b"digraph {\nA -> B\n}", 80) else {
            panic!("multiline digraph must render Art, not Fallback");
        };
        assert_eq!(art_text(&art), "A -> B");
    }

    #[test]
    fn single_line_digraph_renders_both_nodes() {
        let DiagramOutcome::Art(art) = render(b"digraph { A -> B }", 80) else {
            panic!("single-line digraph must render Art with A and B");
        };
        assert_eq!(art_text(&art), "A -> B");
    }

    #[test]
    fn strict_digraph_renders_art() {
        let DiagramOutcome::Art(art) = render(b"strict digraph { A -> B }", 80) else {
            panic!("strict digraph must render Art, not Fallback");
        };
        assert_eq!(art_text(&art), "A -> B");
    }

    #[test]
    fn undirected_graph_renders_art() {
        let DiagramOutcome::Art(art) = render(b"graph { A -- B }", 80) else {
            panic!("undirected graph must render Art, not Fallback");
        };
        let text = art_text(&art);
        assert!(text.contains('A'), "undirected art keeps A: {text}");
        assert!(text.contains('B'), "undirected art keeps B: {text}");
    }

    #[test]
    fn concatenated_graphs_fall_back() {
        assert!(matches!(
            render(b"graph { A -> B } graph { C -> D }", 80),
            DiagramOutcome::Fallback { .. }
        ));
    }

    #[test]
    fn escaped_html_label_falls_back() {
        assert!(matches!(
            render(b"digraph { A [label=&lt;b&gt;] }", 80),
            DiagramOutcome::Fallback { .. }
        ));
    }
}
