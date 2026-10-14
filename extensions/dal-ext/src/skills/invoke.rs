//! Skill invocation: the `/skill` lookup, the `/skill:<name>` commands, and
//! the turn hook that adds invoked bodies.
//!
//! One path serves every form. A prompt names a skill with a whole
//! whitespace-delimited token: `$name`, `$plugin:name`, or `/skill:name`.
//! The before-turn hook reads those tokens off the prompt text and returns
//! each named body once, in order of first mention, as the turn's added
//! text; the user entry keeps what was typed. `/skill:<name> text` and an
//! exact `/skill <name>` only submit that same prompt, so a typed `$name`
//! and a command line reach the model identically.
//!
//! `/skill <prefix>` never resolves a prefix: it lists the skills whose name
//! contains the text and runs nothing. Only an exact name runs a skill.

use std::sync::Arc;

use dal_agent::error::ServiceError;
use dal_agent::ext::{
    BoxFuture, CommandCx, CommandHandler, ExtensionBuilder, Hook, HookCx, HookError,
};
use dal_core::ext::BeforeTurn;
use dal_core::{Command, CommandName, CommandSpec, Expect, Output, Part, RegistrationError, Reply};

use super::{RegisteredSkill, SkillRegistry};

/// Widest one-line description in a `/skill` listing, in bytes.
const LISTING_LINE_BYTES: usize = 120;

/// Widest summary a skill command carries, in bytes.
const SUMMARY_BYTES: usize = 80;

impl SkillRegistry {
    /// The skills whose name contains `query`, in bytewise name order.
    ///
    /// Matching is a plain substring test: no case folding and no fuzzy
    /// matching. An empty `query` names every skill.
    #[must_use]
    pub fn candidates(&self, query: &str) -> Vec<&RegisteredSkill> {
        self.skills
            .iter()
            .filter(|skill| skill.name.contains(query))
            .collect()
    }

    /// The skill one whole prompt token names, if any.
    ///
    /// `$name` and `/skill:name` name a skill by its exact name;
    /// `$plugin:name` also requires `plugin` to be the owner.
    fn named(&self, token: &str) -> Option<&RegisteredSkill> {
        if let Some(name) = token.strip_prefix("/skill:") {
            return self.find(name);
        }
        let rest = token.strip_prefix('$')?;
        match rest.split_once(':') {
            Some((plugin, name)) => self.find(name).filter(|skill| &*skill.plugin == plugin),
            None => self.find(rest),
        }
    }

    /// Every skill the prompt `text` names, once each, in order of first
    /// mention. Tokens that are not whole skill references are plain text.
    #[must_use]
    pub fn invoked(&self, text: &str) -> Vec<&RegisteredSkill> {
        let mut named: Vec<&RegisteredSkill> = Vec::new();
        for skill in text
            .split_whitespace()
            .filter_map(|token| self.named(token))
        {
            if !named.iter().any(|known| known.name == skill.name) {
                named.push(skill);
            }
        }
        named
    }
}

/// The turn text that carries each invoked skill's body, or `None` when the
/// prompt named no skill.
///
/// The body is the exact text `read` serves at `skill://<name>`.
#[must_use]
pub(super) fn reminder(skills: &[&RegisteredSkill]) -> Option<String> {
    if skills.is_empty() {
        return None;
    }
    let mut out = String::new();
    for skill in skills {
        out.push_str("<skill name=\"");
        out.push_str(&skill.name);
        out.push_str("\">\n");
        out.push_str(&skill.body);
        if !skill.body.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("</skill>\n");
    }
    Some(out)
}

/// `text` cut to at most `limit` bytes on a character boundary, ending in `…`
/// when cut. A description is one line: validation rejects control characters.
fn cut(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// The `/skill` reply for a prefix that is not an exact name: one line per
/// candidate, and the statement that nothing ran.
#[must_use]
pub(super) fn listing(registry: &SkillRegistry, query: &str) -> String {
    if registry.skills.is_empty() {
        return "No skills are loaded.".to_owned();
    }
    let found = registry.candidates(query);
    if found.is_empty() {
        return format!("No skill name contains \"{query}\". Nothing was run.");
    }
    let mut out = if query.is_empty() {
        format!("Skills ({}):", found.len())
    } else {
        format!("Skills matching \"{query}\" ({}):", found.len())
    };
    for skill in found {
        out.push_str("\n- ");
        out.push_str(&skill.name);
        out.push_str(": ");
        out.push_str(&cut(&skill.description, LISTING_LINE_BYTES));
    }
    out.push_str("\nNothing was run. Run one with /skill:<name> or $<name>.");
    out
}

/// Submits the prompt that invokes skill `name`, with `text` after it.
async fn invoke(cx: &CommandCx<'_>, name: &str, text: &str) -> Result<Reply, ServiceError> {
    let line = if text.is_empty() {
        format!("/skill:{name}")
    } else {
        format!("/skill:{name} {text}")
    };
    cx.submit_wait(Command::Prompt {
        expect: Expect::Idle,
        content: vec![Part::Text {
            text: line.into_boxed_str(),
        }],
    })
    .await
    .map_err(|error| ServiceError::failed(None, error.to_string()))?;
    Ok(Reply::Done(Output::Nothing))
}

/// `/skill <name>`: an exact name runs that skill; anything else lists
/// candidates and runs nothing.
struct SkillLookup {
    registry: Arc<SkillRegistry>,
}

impl CommandHandler for SkillLookup {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move {
            let (query, text) = args
                .split_once(char::is_whitespace)
                .map_or((args, ""), |(query, text)| (query, text.trim_start()));
            match self.registry.find(query) {
                Some(skill) => invoke(&cx, &skill.name, text).await,
                None => Ok(Reply::Done(Output::Text(
                    listing(&self.registry, query).into_boxed_str(),
                ))),
            }
        })
    }
}

/// `/skill:<name> [text]`: runs one skill.
struct SkillInvoke {
    name: Box<str>,
}

impl CommandHandler for SkillInvoke {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async move { invoke(&cx, &self.name, args).await })
    }
}

/// The before-turn hook: adds the body of every skill the prompt names.
struct InvokedSkills {
    registry: Arc<SkillRegistry>,
}

impl Hook<BeforeTurn, Option<String>> for InvokedSkills {
    fn call(
        &self,
        event: BeforeTurn,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let added = reminder(&self.registry.invoked(&event.text));
        Box::pin(async move { Ok(added) })
    }
}

/// Registers `/skill`, one `/skill:<name>` per skill, and the before-turn
/// hook on `builder`.
///
/// # Errors
///
/// Returns the registration error for a name the command grammar rejects.
pub(super) fn register(
    mut builder: ExtensionBuilder,
    registry: Arc<SkillRegistry>,
) -> Result<ExtensionBuilder, RegistrationError> {
    builder = builder.command(
        CommandSpec {
            name: CommandName::parse("skill")?,
            summary: "list skills by name; an exact name runs it".into(),
            args_hint: Some("[name]".into()),
        },
        Arc::new(SkillLookup {
            registry: Arc::clone(&registry),
        }),
    );
    for skill in registry.iter() {
        builder = builder.command(
            CommandSpec {
                name: CommandName::parse(&format!("skill:{}", skill.name))?,
                summary: cut(&skill.description, SUMMARY_BYTES).into_boxed_str(),
                args_hint: Some("[text]".into()),
            },
            Arc::new(SkillInvoke {
                name: skill.name.clone(),
            }),
        );
    }
    Ok(builder.on_before_turn(InvokedSkills { registry }))
}
