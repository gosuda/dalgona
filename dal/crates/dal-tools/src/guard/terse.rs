use dal_agent::ext::ToolDescription;

pub(super) const ADVISORY: [&str; 4] = [
    "be thorough",
    "consider edge cases",
    "ensure future-proof",
    "handle all cases",
];

pub(super) fn terse(description: &str) -> String {
    let mut kept = Vec::new();
    let mut start = 0;
    let mut chars = description.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if character != '.' {
            continue;
        }
        let end = index.saturating_add(character.len_utf8());
        let separator_end = match chars.peek() {
            Some((next, ' ' | '\n')) => next.saturating_add(1),
            None if end == description.len() => end,
            _ => continue,
        };
        let sentence = &description[start..end];
        if !contains_advisory(sentence) {
            kept.push(sentence);
        }
        start = separator_end;
        while chars.peek().is_some_and(|(next, _)| *next < start) {
            chars.next();
        }
    }
    if start < description.len() {
        let sentence = &description[start..];
        if !contains_advisory(sentence) {
            kept.push(sentence);
        }
    }
    kept.join(" ")
}

fn contains_advisory(sentence: &str) -> bool {
    let bytes = sentence.as_bytes();
    ADVISORY.iter().any(|phrase| {
        bytes
            .windows(phrase.len())
            .any(|window| window.eq_ignore_ascii_case(phrase.as_bytes()))
    })
}

pub(super) fn section(tools: &[ToolDescription]) -> String {
    let mut lines = Vec::with_capacity(tools.len().saturating_add(3));
    lines.push(super::report::MINIMALISM_RULE.to_owned());
    lines.push(String::new());
    lines.push(super::report::CONTRACTS_HEADER.to_owned());
    lines.extend(
        tools
            .iter()
            .map(|tool| format!("- {}: {}", tool.name, terse(&tool.description))),
    );
    lines.push(super::report::CONTRACTS_FOOTER.to_owned());
    lines.join("\n")
}
