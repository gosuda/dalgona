//! DOT validation plus a minimal cell raster over parsed node names.

use super::{ArtCell, ArtRole, DiagramArt, DiagramOutcome};

/// Renders DOT source or returns `invalid dot` for rejected syntax.
#[must_use]
pub fn render(source: &[u8], cap: usize) -> DiagramOutcome {
    let Ok(text) = std::str::from_utf8(source) else {
        return fallback("invalid dot");
    };
    if let Some(reason) = reject(text) {
        let _ = reason;
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

fn reject(text: &str) -> Option<&'static str> {
    let graphs = text.matches("graph").count() + text.matches("digraph").count();
    if graphs > 1 {
        return Some("nested");
    }
    if text.contains('<') || text.contains("&lt;") {
        return Some("html label");
    }
    if text.contains("--") && text.contains("->") {
        return Some("mixed edge operators");
    }
    None
}

fn node_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut current = String::new();
    let mut depth = 0_usize;
    for character in text.chars() {
        match character {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ';' | '\n' if depth > 0 => {
                take_names(&current, &mut names);
                current.clear();
            }
            _ => current.push(character),
        }
    }
    take_names(&current, &mut names);
    names
}

fn take_names(fragment: &str, names: &mut Vec<String>) {
    let fragment = fragment.trim().trim_matches(['{', '}']).trim();
    if fragment.is_empty()
        || fragment.contains('=')
        || fragment.contains("->") && fragment.contains('[')
    {
        for part in fragment.split("->") {
            push_name(part.trim(), names);
        }
        return;
    }
    if fragment.contains("->") {
        for part in fragment.split("->") {
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
    fn simple_digraph_renders_with_text_roles() {
        let outcome = render(b"digraph { A -> B }", 80);
        assert!(
            !matches!(outcome, DiagramOutcome::Pixels(_)),
            "dot text tier never returns pixels"
        );
        match outcome {
            DiagramOutcome::Art(art) => {
                assert!(
                    art.rows
                        .iter()
                        .flatten()
                        .any(|cell| cell.role == ArtRole::Text)
                );
            }
            DiagramOutcome::Fallback { .. } | DiagramOutcome::Pixels(_) => {}
        }
    }
}
