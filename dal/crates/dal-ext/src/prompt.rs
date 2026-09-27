//! Pure, byte-stable system-prompt composition.

pub(crate) mod instructions;

/// The fixed diagram preference in the stable prompt prefix.
pub const D2_PREFERENCE_LINE: &str = "D2 is preferred for diagrams.";
/// The documentation advertisement used when the host supplies no override.
pub const DEFAULT_DOCS_LINE: &str =
    "Read dalgon://index with the read tool when the user asks about dalgon itself.";

/// Borrowed sections for a deterministic system prompt.
#[derive(Debug)]
pub struct PromptInput<'a> {
    /// Product name in the identity paragraph.
    pub product_name: &'a str,
    /// Product version in the identity paragraph.
    pub product_version: &'a str,
    /// Tool descriptions in host-defined order.
    pub tool_lines: &'a [Box<str>],
    /// Skill content, retained without escaping or trimming.
    pub skills: Option<&'a str>,
    /// Always-applied rule content.
    pub rules: Option<&'a str>,
    /// Named rule descriptions.
    pub rulebook: Option<&'a str>,
    /// Plugin names and bodies, sorted by name when emitted.
    pub plugin_sections: &'a [(Box<str>, Box<str>)],
    /// Environment keys and values in host-defined order.
    pub environment: &'a [(Box<str>, Box<str>)],
    /// Rendered project instructions.
    pub instructions: Option<&'a str>,
    /// Unlabelled system instruction content.
    pub system_md: Option<&'a str>,
    /// Descriptions that could not be rendered as images.
    pub letter_fallback: &'a [Box<str>],
    /// Documentation override; `None` selects the default and an empty line omits it.
    pub docs_line: Option<&'a str>,
}

fn section(sink: &mut impl FnMut(&str), tag: &str, body: &str) {
    if body.is_empty() {
        return;
    }
    sink("<");
    sink(tag);
    sink(">\n");
    sink(body);
    if !body.ends_with('\n') {
        sink("\n");
    }
    sink("</");
    sink(tag);
    sink(">\n");
}

fn emit_prefix<'a>(
    input: &PromptInput<'_>,
    plugins: impl Iterator<Item = &'a (Box<str>, Box<str>)>,
    sink: &mut impl FnMut(&str),
) {
    sink("You are ");
    sink(input.product_name);
    sink(" ");
    sink(input.product_version);
    sink(
        ", a coding agent. Work in the current directory. Read files before you edit them. Keep responses short and factual. Finish the whole task before you report.\n",
    );
    sink(D2_PREFERENCE_LINE);
    sink("\n");

    if !input.tool_lines.is_empty() {
        sink("<tools>\n");
        for line in input.tool_lines {
            sink("- ");
            sink(line);
            sink("\n");
        }
        sink("</tools>\n");
    }
    section(sink, "skills", input.skills.unwrap_or(""));
    section(sink, "rules", input.rules.unwrap_or(""));
    section(sink, "rulebook", input.rulebook.unwrap_or(""));

    let mut plugins = plugins.filter(|(_, body)| !body.is_empty()).peekable();
    if plugins.peek().is_some() {
        sink("<plugins>\n");
        for (name, body) in plugins {
            sink("plugin '");
            sink(name);
            sink("':\n");
            sink(body);
            if !body.ends_with('\n') {
                sink("\n");
            }
        }
        sink("</plugins>\n");
    }
}

fn emit_suffix(input: &PromptInput<'_>, sink: &mut impl FnMut(&str)) {
    if !input.environment.is_empty() {
        sink("<environment>\n");
        for (key, value) in input.environment {
            sink(key);
            sink(": ");
            sink(value);
            sink("\n");
        }
        sink("</environment>\n");
    }

    let instructions = input.instructions.filter(|text| !text.is_empty());
    let system_md = input.system_md.filter(|text| !text.is_empty());
    if instructions.is_some() || system_md.is_some() {
        sink("<instructions>\n");
        if let Some(body) = instructions {
            sink(body);
        }
        if let Some(body) = system_md {
            if let Some(previous) = instructions {
                let trailing = previous
                    .bytes()
                    .rev()
                    .take(2)
                    .take_while(|&b| b == b'\n')
                    .count();
                let leading = body.bytes().take(2).take_while(|&b| b == b'\n').count();
                sink(&"\n\n"[(trailing + leading).min(2)..]);
            }
            sink(body);
        }
        if !system_md
            .or(instructions)
            .is_some_and(|body| body.ends_with('\n'))
        {
            sink("\n");
        }
        sink("</instructions>\n");
    }

    if input.letter_fallback.iter().any(|text| !text.is_empty()) {
        sink("<letter-fallback>\n");
        let mut first = true;
        for text in input.letter_fallback.iter().filter(|text| !text.is_empty()) {
            if !first {
                sink("\n");
            }
            sink(text);
            first = false;
        }
        let last_nonempty = input
            .letter_fallback
            .iter()
            .rev()
            .find(|text| !text.is_empty());
        if !last_nonempty.is_some_and(|text| text.ends_with('\n')) {
            sink("\n");
        }
        sink("</letter-fallback>\n");
    }

    let docs_line = input.docs_line.unwrap_or(DEFAULT_DOCS_LINE);
    if !docs_line.is_empty() {
        sink(docs_line);
        sink("\n");
    }
}

/// Builds a prompt with a stable prefix and a per-interaction suffix.
#[must_use]
pub fn build(input: &PromptInput<'_>) -> String {
    let mut prompt = String::new();
    let mut plugins: Vec<_> = input.plugin_sections.iter().collect();
    plugins.sort_by(|left, right| left.0.cmp(&right.0));
    {
        let mut sink = |chunk: &str| prompt.push_str(chunk);
        emit_prefix(input, plugins.into_iter(), &mut sink);
        emit_suffix(input, &mut sink);
    }
    prompt
}

/// Counts the stable prefix in UTF-8 bytes without allocating or sorting.
#[must_use]
pub fn prefix_bytes(input: &PromptInput<'_>) -> usize {
    let mut bytes = 0;
    let mut sink = |chunk: &str| bytes += chunk.len();
    emit_prefix(input, input.plugin_sections.iter(), &mut sink);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        tool_lines: &'a [Box<str>],
        plugin_sections: &'a [(Box<str>, Box<str>)],
        environment: &'a [(Box<str>, Box<str>)],
        letter_fallback: &'a [Box<str>],
    ) -> PromptInput<'a> {
        PromptInput {
            product_name: "dal",
            product_version: "0.1",
            tool_lines,
            skills: Some("skill body\nkept as supplied"),
            rules: Some("rule body"),
            rulebook: Some("rulebook body"),
            plugin_sections,
            environment,
            instructions: Some("# Instructions from /project/AGENTS.md\nDo this"),
            system_md: Some("SYSTEM content"),
            letter_fallback,
            docs_line: None,
        }
    }

    #[test]
    fn composition_uses_order_and_sorted_plugin_names() {
        let plugins = [
            (Box::from("zeta"), Box::from("zeta body")),
            (Box::from("alpha"), Box::from("alpha body")),
        ];
        let environment = [(Box::from("cwd"), Box::from("/work"))];
        let tools = [Box::from("read <path>"), Box::from("search <query>")];
        let fallbacks = [Box::from("fallback description")];
        let prompt = build(&input(&tools, &plugins, &environment, &fallbacks));

        let order = [
            "You are dal 0.1",
            D2_PREFERENCE_LINE,
            "- read <path>",
            "<skills>",
            "<rules>",
            "<rulebook>",
            "plugin 'alpha':",
            "plugin 'zeta':",
            "<environment>",
            "<instructions>",
            "<letter-fallback>",
            DEFAULT_DOCS_LINE,
        ];
        let mut previous = 0;
        for marker in order {
            let next = prompt[previous..]
                .find(marker)
                .expect("expected composition marker")
                + previous;
            assert!(next >= previous);
            previous = next + marker.len();
        }
        assert!(prompt.contains("skill body\nkept as supplied"));
        assert!(prompt.contains("<instructions>\n# Instructions from /project/AGENTS.md\nDo this\n\nSYSTEM content\n</instructions>"));
    }

    #[test]
    fn rendered_instructions_keep_one_separator_before_system_text() {
        let files = [instructions::InstructionFile {
            path: "/project/AGENTS.md".into(),
            content: "Keep supplied content\n".into(),
            truncated: false,
        }];
        let rendered = instructions::render(&files).unwrap();
        let mut sections = input(&[], &[], &[], &[]);
        sections.instructions = Some(&rendered);
        let prompt = build(&sections);
        assert!(prompt.contains(
            "<instructions>\n# Instructions from /project/AGENTS.md\nKeep supplied content\n\nSYSTEM content\n</instructions>"
        ));
        for (body, system, joined) in [
            ("Keep\n", "SYSTEM content", "Keep\n\nSYSTEM content"),
            ("Keep\n\n\n", "SYSTEM content", "Keep\n\n\nSYSTEM content"),
            ("Keep", "\nSYSTEM content", "Keep\n\nSYSTEM content"),
            ("Keep\n", "\nSYSTEM content", "Keep\n\nSYSTEM content"),
        ] {
            sections.instructions = Some(body);
            sections.system_md = Some(system);
            assert!(build(&sections).contains(joined));
        }
    }

    #[test]
    fn prefix_ignores_suffix_inputs_and_matches_emitted_prefix() {
        let plugins = [(Box::from("letters"), Box::from("plugin body"))];
        let first_environment = [(Box::from("cwd"), Box::from("/one"))];
        let second_environment = [(Box::from("cwd"), Box::from("/two"))];
        let tools = [Box::from("read <path>"), Box::from("search <query>")];
        let first_fallbacks = [Box::from("fallback description")];
        let second_fallbacks = [Box::from("different fallback")];
        let first = input(&tools, &plugins, &first_environment, &first_fallbacks);
        let mut second = input(&tools, &plugins, &second_environment, &second_fallbacks);
        second.instructions = Some("changed instructions");
        second.system_md = Some("changed system text");
        second.docs_line = Some("changed docs line");

        let first_prompt = build(&first);
        let second_prompt = build(&second);
        let prefix_len = prefix_bytes(&first);
        assert_eq!(prefix_len, prefix_bytes(&second));
        assert_eq!(
            &first_prompt.as_bytes()[..prefix_len],
            &second_prompt.as_bytes()[..prefix_len]
        );
        assert!(first_prompt[..prefix_len].ends_with("</plugins>\n"));
        assert_ne!(first_prompt, second_prompt);
    }

    #[test]
    fn empty_sections_are_omitted_and_docs_can_be_replaced() {
        let no_plugins: [(Box<str>, Box<str>); 0] = [];
        let no_environment: [(Box<str>, Box<str>); 0] = [];
        let no_tools: [Box<str>; 0] = [];
        let empty_fallback = [Box::from("")];
        let mut empty = input(&no_tools, &no_plugins, &no_environment, &empty_fallback);
        empty.skills = Some("");
        empty.rules = None;
        empty.rulebook = Some("");
        empty.instructions = Some("");
        empty.system_md = None;
        empty.docs_line = Some("custom docs line");

        let prompt = build(&empty);
        for omitted in [
            "<tools>",
            "<skills>",
            "<rules>",
            "<rulebook>",
            "<plugins>",
            "<environment>",
            "<instructions>",
            "<letter-fallback>",
            DEFAULT_DOCS_LINE,
        ] {
            assert!(!prompt.contains(omitted));
        }
        assert!(prompt.ends_with("custom docs line\n"));
    }
}
