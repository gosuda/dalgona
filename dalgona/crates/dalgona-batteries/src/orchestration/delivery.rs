// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Report delivery: bounded text previews of child reports.

/// Cuts a preview at a UTF-8 boundary and marks the cut with `...`.
pub(crate) fn preview(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

#[cfg(test)]
mod tests;
