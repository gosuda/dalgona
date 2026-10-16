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

/// Emits the identity paragraph and the `D2` preference line.
fn emit_head(product_name: &str, product_version: &str, sink: &mut impl FnMut(&str)) {
    sink("You are ");
    sink(product_name);
    sink(" ");
    sink(product_version);
    sink(
        ", a coding agent. Work in the current directory. Read files before you edit them. Keep responses short and factual. Finish the whole task before you report.\n",
    );
    sink(D2_PREFERENCE_LINE);
    sink("\n");
}

/// Emits the `<tools>` block; emits nothing when there are no tool lines.
fn emit_tool_block(tool_lines: &[Box<str>], sink: &mut impl FnMut(&str)) {
    if tool_lines.is_empty() {
        return;
    }
    sink("<tools>\n");
    for line in tool_lines {
        sink("- ");
        sink(line);
        sink("\n");
    }
    sink("</tools>\n");
}

fn emit_prefix<'a>(
    input: &PromptInput<'_>,
    plugins: impl Iterator<Item = &'a (Box<str>, Box<str>)>,
    sink: &mut impl FnMut(&str),
) {
    emit_head(input.product_name, input.product_version, sink);
    emit_tool_block(input.tool_lines, sink);
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

/// Emits the `<instructions>` block; emits nothing when both inputs are empty.
fn emit_instructions_block(
    instructions: Option<&str>,
    system_md: Option<&str>,
    sink: &mut impl FnMut(&str),
) {
    let kept_instructions = instructions.filter(|text| !text.is_empty());
    let kept_system_md = system_md.filter(|text| !text.is_empty());
    if kept_instructions.is_some() || kept_system_md.is_some() {
        sink("<instructions>\n");
        if let Some(body) = kept_instructions {
            sink(body);
        }
        if let Some(body) = kept_system_md {
            if let Some(previous) = kept_instructions {
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
        if !kept_system_md
            .or(kept_instructions)
            .is_some_and(|body| body.ends_with('\n'))
        {
            sink("\n");
        }
        sink("</instructions>\n");
    }
}

/// Emits the docs line; `None` selects the default and an empty line omits it.
fn emit_docs_line(docs_line: Option<&str>, sink: &mut impl FnMut(&str)) {
    let line = docs_line.unwrap_or(DEFAULT_DOCS_LINE);
    if !line.is_empty() {
        sink(line);
        sink("\n");
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

    emit_instructions_block(input.instructions, input.system_md, sink);

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

    emit_docs_line(input.docs_line, sink);
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

/// Runs one emitter into a fresh buffer; `None` when it wrote nothing, so the
/// assembler omits the section instead of joining an empty one.
fn rendered(emit: impl FnOnce(&mut dyn FnMut(&str))) -> Option<String> {
    let mut text = String::new();
    emit(&mut |chunk: &str| text.push_str(chunk));
    (!text.is_empty()).then_some(text)
}

/// The identity paragraph and the `D2` preference line, at
/// [`PromptOrder::Identity`]. Always renders.
#[derive(Clone, Debug)]
struct HeadSection {
    product_name: Box<str>,
    product_version: Box<str>,
}

impl SectionFn for HeadSection {
    fn render(&self, _cx: &SectionCx<'_>) -> Option<String> {
        rendered(|mut sink| {
            emit_head(&self.product_name, &self.product_version, &mut sink);
        })
    }
}

/// The `<tools>` block over the generation's model-visible tools, at
/// [`PromptOrder::Tools`]. Omitted when the generation lists no tool.
#[derive(Clone, Copy, Debug)]
struct ToolsSection;

impl SectionFn for ToolsSection {
    fn render(&self, cx: &SectionCx<'_>) -> Option<String> {
        let tool_lines: Vec<Box<str>> = cx
            .tools
            .iter()
            .map(|tool| format!("{}: {}", tool.name.as_str(), tool.description).into())
            .collect();
        rendered(|mut sink| {
            emit_tool_block(&tool_lines, &mut sink);
        })
    }
}

/// The `<instructions>` block over the host-resolved instructions and
/// `SYSTEM.md` text, at [`PromptOrder::Instructions`]. Omitted when both are
/// absent or empty.
#[derive(Clone, Copy, Debug)]
struct InstructionsSection;

impl SectionFn for InstructionsSection {
    fn render(&self, cx: &SectionCx<'_>) -> Option<String> {
        rendered(|mut sink| {
            emit_instructions_block(cx.instructions, cx.system_md, &mut sink);
        })
    }
}

/// The default docs line, at [`PromptOrder::DocsLine`]. Always renders.
#[derive(Clone, Copy, Debug)]
struct DocsSection;

impl SectionFn for DocsSection {
    fn render(&self, _cx: &SectionCx<'_>) -> Option<String> {
        rendered(|mut sink| {
            emit_docs_line(None, &mut sink);
        })
    }
}

/// Builds the `prompt` extension: four session prompt sections under one
/// owner, each at the documented [`PromptOrder`] of the part it carries.
///
/// | Order | Section |
/// |---|---|
/// | `Identity` | identity paragraph and the `D2` preference line |
/// | `Tools` | the `<tools>` block, omitted without tools |
/// | `Instructions` | the `<instructions>` block, omitted without instructions or `SYSTEM.md` text |
/// | `DocsLine` | the default docs line |
///
/// The assembler sorts every extension's sections by order, so skills, rules,
/// the rulebook, and plugin sections land between the tools block and the
/// instructions block wherever their extensions sit in the catalog. Each
/// section carries the exact bytes [`build`] emits for that part, including
/// its trailing newline; the assembler adds its own separator. The host
/// renders them once at session open and again on generation publish.
///
/// # Errors
///
/// Returns the runtime's typed build error when the builder rejects the
/// registration.
pub fn extension() -> Result<Extension, RegistrationError> {
    ExtensionBuilder::new("prompt", "0.1.0", ServiceSet::EMPTY)?
        .prompt_section(PromptSection::session(
            PromptOrder::Identity,
            Arc::new(HeadSection {
                product_name: "dal".into(),
                product_version: env!("CARGO_PKG_VERSION").into(),
            }),
        ))
        .prompt_section(PromptSection::session(
            PromptOrder::Tools,
            Arc::new(ToolsSection),
        ))
        .prompt_section(PromptSection::session(
            PromptOrder::Instructions,
            Arc::new(InstructionsSection),
        ))
        .prompt_section(PromptSection::session(
            PromptOrder::DocsLine,
            Arc::new(DocsSection),
        ))
        .build()
}

#[cfg(test)]
mod tests;
