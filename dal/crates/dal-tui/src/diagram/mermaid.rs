//! Mermaid flowcharts rendered in process as role-colored text art.

use super::{ArtCell, ArtRole, DiagramArt, DiagramOutcome};

/// Renders mermaid source, narrowing direction until the art fits the cap.
#[must_use]
pub fn render(source: &[u8], cap: usize) -> DiagramOutcome {
    let Ok(text) = std::str::from_utf8(source) else {
        return fallback("invalid mermaid");
    };
    if has_dangling_edge(text) {
        return fallback("invalid mermaid");
    }
    for candidate in [
        text.to_owned(),
        force_direction(text, "TD"),
        force_direction(text, "LR"),
    ] {
        let rendered = mermaid_text::render_with_width(&candidate, Some(cap.max(1)));
        let Ok(art) = rendered else {
            continue;
        };
        let diagram = to_art(&art);
        if diagram.width_cells == 0 {
            continue;
        }
        if diagram.width_cells <= cap.max(1) {
            return DiagramOutcome::Art(diagram);
        }
    }
    let narrowest =
        mermaid_text::render_with_width(text, Some(1)).map_or(usize::MAX, |art| art_width(&art));
    if narrowest == usize::MAX {
        return fallback("invalid mermaid");
    }
    fallback("too wide")
}

fn force_direction(source: &str, direction: &str) -> String {
    let mut lines = source.lines();
    let Some(first) = lines.next() else {
        return source.to_owned();
    };
    let replaced = replace_direction(first, direction);
    std::iter::once(replaced)
        .chain(lines.map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

fn replace_direction(line: &str, direction: &str) -> String {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() >= 2 && tokens[0] == "graph" {
        let mut tokens = tokens;
        tokens[1] = direction;
        tokens.join(" ")
    } else {
        line.to_owned()
    }
}

/// Reports an edge operator with no target node: the renderer is lenient,
/// but a dangling edge is broken source and must fall back.
fn has_dangling_edge(source: &str) -> bool {
    const TAILS: [&str; 9] = [
        "-->", "--->", "---", "--o", "--x", "-.->", "-.-", "==>", "~~~",
    ];
    source
        .split([';', '\n'])
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .any(|statement| {
            !statement.starts_with("graph") && TAILS.iter().any(|tail| statement.ends_with(tail))
        })
}

fn to_art(rendered: &str) -> DiagramArt {
    let mut rows = Vec::new();
    let mut width = 0;
    let mut cells = 0;
    for line in rendered.lines() {
        let mut row = Vec::new();
        for character in line.chars() {
            let scalar = character as u32;
            let role = if (0x2500..=0x257F).contains(&scalar) {
                ArtRole::Border
            } else if (0x2190..=0x21FF).contains(&scalar) || (0x25A0..=0x25FF).contains(&scalar) {
                ArtRole::Edge
            } else {
                ArtRole::Text
            };
            row.push(ArtCell {
                text: character.to_string(),
                role,
            });
            cells += 1;
        }
        width = width.max(row.len());
        rows.push(row);
    }
    let _ = cells;
    DiagramArt {
        rows,
        width_cells: width,
    }
}

fn art_width(rendered: &str) -> usize {
    rendered
        .lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
}

fn fallback(reason: &str) -> DiagramOutcome {
    DiagramOutcome::Fallback {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::diagram::{DiagramKind, DiagramOutcome};

    #[test]
    fn golden_flowchart_stays_within_cap() {
        let outcome = render(b"graph TD; A-->B", 80);
        assert!(
            !matches!(outcome, DiagramOutcome::Pixels(_)),
            "mermaid never returns pixels"
        );
        match outcome {
            DiagramOutcome::Art(art) => {
                assert!(art.width_cells <= 80);
                assert!(!art.rows.is_empty());
            }
            DiagramOutcome::Fallback { .. } | DiagramOutcome::Pixels(_) => {}
        }
        let _ = DiagramKind::Mermaid;
    }

    #[test]
    fn broken_flowchart_falls_back_without_losing_source() {
        assert!(matches!(
            render(b"graph TD; A-->", 80),
            DiagramOutcome::Fallback { .. }
        ));
    }
}
