// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use dal_tools::parse::Language;

/// Returns the 1-based line range and text of `span` when it covers only whole comment lines.
#[must_use]
pub fn whole_lines(bytes: &[u8], span: (u64, u64)) -> Option<(u32, u32, String)> {
    let start = usize::try_from(span.0).ok()?;
    let end = usize::try_from(span.1).ok()?;
    if start > end || end > bytes.len() || start == end {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    let span_text = text[start..end].to_owned();

    let line_start = text[..start].matches('\n').count() + 1;
    let last_byte = end.saturating_sub(1);
    let line_end = text[..last_byte.saturating_add(1)].matches('\n').count() + 1;

    let lines: Vec<&str> = text.lines().collect();
    if line_start == 0 || line_end == 0 || line_end > lines.len() || line_start > line_end {
        return None;
    }
    let first = lines[line_start - 1].trim_start_matches([' ', '\t']);
    let last = lines[line_end - 1].trim_start_matches([' ', '\t']);
    let last_trimmed_end = last.trim_end_matches([' ', '\t', '\r']);

    if starts_with_line_comment(first) {
        for line in &lines[line_start - 1..line_end] {
            let trimmed = line.trim_start_matches([' ', '\t']);
            if !starts_with_line_comment(trimmed) {
                return None;
            }
        }
    } else if starts_with_block_open(first) {
        if !ends_with_block_close(last_trimmed_end) {
            return None;
        }
    } else {
        return None;
    }

    let line_start = u32::try_from(line_start).ok()?;
    let line_end = u32::try_from(line_end).ok()?;
    Some((line_start, line_end, span_text))
}

fn starts_with_line_comment(line: &str) -> bool {
    line.starts_with("//") || line.starts_with('#')
}

fn starts_with_block_open(line: &str) -> bool {
    line.starts_with("(*") || line.starts_with("/*")
}

fn ends_with_block_close(line: &str) -> bool {
    line.ends_with("*)") || line.ends_with("*/")
}

/// Returns the 1-based line range and text of `span` when it occupies complete source lines.
#[must_use]
pub fn whole_source_lines(bytes: &[u8], span: (u64, u64)) -> Option<(u32, u32, String)> {
    let start = usize::try_from(span.0).ok()?;
    let end = usize::try_from(span.1).ok()?;
    if start >= end || end > bytes.len() {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    let start_line_start = text[..start].rfind('\n').map_or(0, |index| index + 1);
    let start_prefix = text[start_line_start..start].trim_matches([' ', '\t', '\r']);
    if !start_prefix.is_empty() {
        return None;
    }
    let end_suffix = if bytes.get(end.saturating_sub(1)) == Some(&b'\n') {
        ""
    } else {
        let end_line_end = text[end..]
            .find('\n')
            .map_or(text.len(), |index| end + index);
        text[end..end_line_end].trim_matches([' ', '\t', '\r'])
    };
    if !end_suffix.is_empty() {
        return None;
    }
    let line_start = u32::try_from(text[..start].matches('\n').count().saturating_add(1)).ok()?;
    let line_end = text[..end]
        .matches('\n')
        .count()
        .saturating_add(usize::from(!bytes[..end].ends_with(b"\n")));
    let line_end = u32::try_from(line_end).ok()?;
    Some((line_start, line_end, text[start..end].to_owned()))
}

/// Builds the replacement text that turns an empty catch into a rethrow.
#[must_use]
pub fn rethrow_after(codemod_lang: Language, captured_node_text: &str) -> String {
    match codemod_lang {
        Language::Python => {
            let Some(colon) = captured_node_text.find(':') else {
                return format!("{captured_node_text} raise\n");
            };
            let header = &captured_node_text[..=colon];
            format!("{header} raise\n")
        }
        Language::Cpp | Language::C => "catch (...) { throw; }\n".to_owned(),
        Language::Ocaml | Language::OcamlInterface => "| exn -> raise exn\n".to_owned(),
        _ => String::new(),
    }
}
