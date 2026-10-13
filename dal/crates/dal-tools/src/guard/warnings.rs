use regex::Regex;
use std::sync::LazyLock;

static OCAML_LOCATION: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r#"File "([^"]+)", line (\d+)"#));
static RUST_WARNING: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"^warning: (unused|variable .* is never used|function .* is never used|variant .* is never constructed|field .* is never read)",
    )
});
static RUST_LOCATION: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^\s*--> ([^:]+):(\d+):\d+"));
static GENERIC_LOCATION: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"([^\s:]+\.[A-Za-z0-9]+):(\d+)"));

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Warning {
    pub path: Box<str>,
    pub line: u32,
    pub text: Box<str>,
}

impl Warning {
    pub(super) fn key(&self) -> (Box<str>, Box<str>) {
        let text = self
            .text
            .chars()
            .filter(|character| !character.is_ascii_digit())
            .collect();
        (self.path.clone(), text)
    }
}

pub(super) fn full_output_path(preview: &str) -> Option<&str> {
    preview.lines().find_map(|line| {
        line.strip_prefix("Full output: ")
            .filter(|path| !path.is_empty())
    })
}

pub(super) fn grep_like(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if matches!(first, "grep" | "rg" | "ag" | "ack") {
        return true;
    }
    first == "git" && words.next() == Some("grep")
}

pub(super) fn scan(output: &str) -> Vec<Warning> {
    let lines: Vec<_> = output.lines().collect();
    let mut warnings = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let location = if contains_ocaml_warning(text) {
            previous_ocaml_location(&lines, index)
        } else if captures(&RUST_WARNING, text).is_some() {
            next_rust_location(&lines, index)
        } else if contains_generic_warning(text) {
            generic_location(text)
        } else {
            None
        };
        if let Some((path, line)) = location {
            warnings.push(Warning {
                path: path.into(),
                line,
                text: text.into(),
            });
        }
    }
    warnings
}

fn contains_ocaml_warning(line: &str) -> bool {
    ["Warning 26", "Warning 27", "Warning 32", "Warning 8"]
        .iter()
        .any(|warning| line.contains(warning))
}

fn contains_generic_warning(line: &str) -> bool {
    [
        "unused variable",
        "unused function",
        "unreachable code",
        "assigned but never used",
    ]
    .iter()
    .any(|warning| line.contains(warning))
}

fn captures<'a>(
    pattern: &LazyLock<Result<Regex, regex::Error>>,
    text: &'a str,
) -> Option<regex::Captures<'a>> {
    pattern.as_ref().as_ref().ok()?.captures(text)
}

fn captured_path_line(captures: &regex::Captures<'_>) -> Option<(String, u32)> {
    let path = captures.get(1)?.as_str().to_owned();
    let line = captures.get(2)?.as_str().parse().ok()?;
    Some((path, line))
}

fn previous_ocaml_location(lines: &[&str], warning: usize) -> Option<(String, u32)> {
    let start = warning.saturating_sub(4);
    lines[start..warning].iter().rev().find_map(|line| {
        captures(&OCAML_LOCATION, line).and_then(|found| captured_path_line(&found))
    })
}

fn next_rust_location(lines: &[&str], warning: usize) -> Option<(String, u32)> {
    let start = warning.saturating_add(1);
    let end = warning.saturating_add(5).min(lines.len());
    lines.get(start..end)?.iter().find_map(|line| {
        captures(&RUST_LOCATION, line).and_then(|found| captured_path_line(&found))
    })
}

fn generic_location(line: &str) -> Option<(String, u32)> {
    captures(&GENERIC_LOCATION, line).and_then(|found| captured_path_line(&found))
}
