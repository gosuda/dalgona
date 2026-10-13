//! Pure, byte-stable system-prompt composition.

use std::sync::Arc;

use dal_agent::ext::{
    Extension, ExtensionBuilder, PromptOrder, PromptSection, SectionCx, SectionFn,
};
use dal_core::{RegistrationError, ServiceSet};

pub(crate) mod instructions;

/// The fixed diagram preference in the stable prompt prefix.
pub const D2_PREFERENCE_LINE: &str = "D2 is preferred for diagrams.";
/// The documentation advertisement used when the host supplies no override.
pub const DEFAULT_DOCS_LINE: &str =
    "Read dal://index with the read tool when the user asks about dal itself.";

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

/// Session prompt section combining the stable identity/tools prefix with the
/// host-resolved instructions and `SYSTEM.md` content.
///
/// Skills, rules, plugin sections, environment, fallback text, and the docs
/// line ride their own extensions and host inputs; this section contributes
/// identity, the diagram preference, generation tool lines, instructions,
/// and system text, and omits nothing it owns (the identity paragraph always
/// renders).
#[derive(Clone, Debug)]
struct PromptSectionFn {
    product_name: Box<str>,
    product_version: Box<str>,
}

impl SectionFn for PromptSectionFn {
    fn render(&self, cx: &SectionCx<'_>) -> Option<String> {
        let tool_lines: Vec<Box<str>> = cx
            .tools
            .iter()
            .map(|tool| format!("{}: {}", tool.name.as_str(), tool.description).into())
            .collect();
        let empty: [(Box<str>, Box<str>); 0] = [];
        let no_fallback: [Box<str>; 0] = [];
        let input = PromptInput {
            product_name: &self.product_name,
            product_version: &self.product_version,
            tool_lines: &tool_lines,
            skills: None,
            rules: None,
            rulebook: None,
            plugin_sections: &empty,
            environment: &empty,
            instructions: cx.instructions,
            system_md: cx.system_md,
            letter_fallback: &no_fallback,
            docs_line: None,
        };
        Some(build(&input))
    }
}

/// Builds the `prompt` extension: one session prompt section at the identity
/// order. The host renders it once at session open and again on generation
/// publish; skills, rules, and plugin content arrive through their own
/// sections.
///
/// # Errors
///
/// Returns the runtime's typed build error when the builder rejects the
/// registration.
pub fn extension() -> Result<Extension, RegistrationError> {
    let section = PromptSection::session(
        PromptOrder::Identity,
        Arc::new(PromptSectionFn {
            product_name: "dal".into(),
            product_version: env!("CARGO_PKG_VERSION").into(),
        }),
    );
    ExtensionBuilder::new("prompt", "0.1.0", ServiceSet::EMPTY)?
        .prompt_section(section)
        .build()
}

#[cfg(test)]
mod tests;
