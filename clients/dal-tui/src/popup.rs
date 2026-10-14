//! Completion popup over `Host::commands` entries and `@` files.

use dal_core::CommandSpec;

use crate::composer::slash_completion;

/// The completion lane a candidate belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lane {
    /// `/` lists commands; `skill:` entries never appear here.
    Slash,
    /// `$` lists skills and nothing else.
    Skill,
}

impl Lane {
    /// The character that opens the lane.
    #[must_use]
    pub const fn sigil(self) -> char {
        match self {
            Self::Slash => '/',
            Self::Skill => '$',
        }
    }
}

/// One completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The lane the candidate completes in.
    pub lane: Lane,
    /// Command name without the leading slash, or skill name without the
    /// leading dollar sign.
    pub name: String,
    /// Short summary.
    pub summary: String,
}

/// Matches `entries` by name prefix and orders them by name: the one match
/// and ranking rule both lanes share.
fn ranked<'a>(
    lane: Lane,
    entries: impl Iterator<Item = (&'a str, &'a CommandSpec)>,
    prefix: &str,
) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = entries
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, spec)| Candidate {
            lane,
            name: name.to_owned(),
            summary: spec.summary.to_string(),
        })
        .collect();
    candidates.sort_by(|left, right| left.name.cmp(&right.name));
    candidates
}

/// Filters command specs by a `/` prefix; empty command lists render nothing.
/// `skill:` entries are skills, not commands, and never appear.
#[must_use]
pub fn complete_commands(commands: &[CommandSpec], prefix: &str) -> Vec<Candidate> {
    let entries = commands
        .iter()
        .map(|spec| (spec.name.as_str(), spec))
        .filter(|(name, _)| !name.starts_with("skill:"));
    ranked(Lane::Slash, entries, prefix)
}

/// Filters the skills among the command specs by a `$` prefix, with the
/// same match and ranking as [`complete_commands`].
#[must_use]
pub fn complete_skills(commands: &[CommandSpec], prefix: &str) -> Vec<Candidate> {
    let entries = commands
        .iter()
        .filter_map(|spec| Some((spec.name.as_str().strip_prefix("skill:")?, spec)));
    ranked(Lane::Skill, entries, prefix)
}

/// The final whitespace-delimited token of `draft` when it opens the skills
/// lane: its byte offset and the text after the `$`.
///
/// A draft that is a slash command lexes its arguments for the host instead,
/// except a `/skill:` line, which takes more skills in its text.
fn skill_token(draft: &str) -> Option<(usize, &str)> {
    let start = draft
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map_or(0, |(at, character)| at + character.len_utf8());
    let prefix = draft[start..].strip_prefix('$')?;
    let command = draft
        .strip_prefix('/')
        .map(|body| body.split(char::is_whitespace).next().unwrap_or(""));
    command
        .is_none_or(|name| name.starts_with("skill:"))
        .then_some((start, prefix))
}

/// What a full composer draft completes to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DraftCompletion {
    /// Complete a command or skill name from the registry.
    Names(Vec<Candidate>),
    /// Complete the argument from the host feed matching `prefix`.
    Args {
        /// The command name as written, without the leading slash.
        name: String,
        /// The partial argument being typed.
        prefix: String,
    },
}

/// Completes a full composer draft through the fixed command lexer.
///
/// A final token that starts with `$` opens the skills-only lane. Other
/// drafts still on the command name filter the registry, including
/// `plugin:command` names; drafts with argument text return the lexed name
/// and partial argument for host-side value completion. Plain text drafts
/// complete nothing.
#[must_use]
pub fn complete_draft(commands: &[CommandSpec], draft: &str) -> DraftCompletion {
    if let Some((_, prefix)) = skill_token(draft) {
        return DraftCompletion::Names(complete_skills(commands, prefix));
    }
    let Some(body) = draft.strip_prefix('/') else {
        return DraftCompletion::Names(Vec::new());
    };
    if !body.contains([' ', '\t']) {
        if body.is_empty() {
            return DraftCompletion::Names(complete_commands(commands, ""));
        }
        let Some(split) = slash_completion(draft) else {
            return DraftCompletion::Names(Vec::new());
        };
        return DraftCompletion::Names(complete_commands(commands, &split.name));
    }
    let Some(split) = slash_completion(draft) else {
        return DraftCompletion::Names(Vec::new());
    };
    DraftCompletion::Args {
        name: split.name,
        prefix: split.prefix,
    }
}

/// The draft after accepting `candidate`.
///
/// A command replaces the whole draft; a skill replaces only the `$` token
/// being typed and keeps the rest of the prompt.
#[must_use]
pub fn accept(draft: &str, candidate: &Candidate) -> String {
    match candidate.lane {
        Lane::Slash => format!("/{} ", candidate.name),
        Lane::Skill => {
            let start = skill_token(draft).map_or(draft.len(), |(start, _)| start);
            format!("{}${} ", &draft[..start], candidate.name)
        }
    }
}

#[cfg(test)]
mod tests {
    use dal_core::{CommandName, CommandSpec};

    use super::{
        DraftCompletion, Lane, accept, complete_commands, complete_draft, complete_skills,
    };

    fn specs(names: &[&str]) -> Vec<CommandSpec> {
        names
            .iter()
            .map(|name| CommandSpec {
                name: CommandName::parse(name).unwrap(),
                summary: format!("{name} summary").into(),
                args_hint: None,
            })
            .collect()
    }

    fn registry() -> Vec<CommandSpec> {
        specs(&["model", "quality:todos"])
    }

    fn with_skills() -> Vec<CommandSpec> {
        specs(&[
            "model",
            "skill",
            "skill:review",
            "skill:find-anything",
            "skill:find-files",
            "scoped-models",
        ])
    }

    fn names(completion: DraftCompletion) -> Vec<String> {
        match completion {
            DraftCompletion::Names(candidates) => {
                candidates.into_iter().map(|item| item.name).collect()
            }
            DraftCompletion::Args { name, prefix } => {
                panic!("expected names, got args {name:?} {prefix:?}")
            }
        }
    }

    #[test]
    fn empty_command_list_renders_nothing() {
        assert_eq!(complete_commands(&[], "mod"), []);
        assert_eq!(complete_skills(&[], ""), []);
    }

    #[test]
    fn draft_completion_lists_names_and_plugin_commands() {
        let commands = registry();
        assert!(matches!(
            complete_draft(&commands, "/"),
            DraftCompletion::Names(names)
                if names.iter().any(|item| item.name == "model")
                    && names.iter().any(|item| item.name == "quality:todos")
        ));
        assert!(matches!(
            complete_draft(&commands, "/quality"),
            DraftCompletion::Names(names)
                if names.iter().any(|item| item.name == "quality:todos")
        ));
        assert!(matches!(
            complete_draft(&commands, "/model src"),
            DraftCompletion::Args { name, prefix }
                if name == "model" && prefix == "src"
        ));
    }

    #[test]
    fn draft_completion_ignores_plain_text() {
        let commands = registry();
        assert!(matches!(
            complete_draft(&commands, "hello"),
            DraftCompletion::Names(names) if names.is_empty()
        ));
        assert!(matches!(
            complete_draft(&commands, "/skill:"),
            DraftCompletion::Names(names) if names.is_empty()
        ));
    }

    #[test]
    fn slash_lane_never_lists_skill_entries() {
        let commands = with_skills();
        assert_eq!(
            names(complete_draft(&commands, "/")),
            ["model", "scoped-models", "skill"]
        );
        assert_eq!(
            names(complete_draft(&commands, "/s")),
            ["scoped-models", "skill"]
        );
        assert_eq!(
            names(complete_draft(&commands, "/skill:")),
            Vec::<String>::new()
        );
        assert_eq!(
            names(complete_draft(&commands, "/skill:rev")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn dollar_lane_lists_only_skills_in_name_order() {
        let commands = with_skills();
        assert_eq!(
            names(complete_draft(&commands, "$")),
            ["find-anything", "find-files", "review"]
        );
        assert_eq!(
            names(complete_draft(&commands, "$find")),
            ["find-anything", "find-files"]
        );
        assert_eq!(
            names(complete_draft(&commands, "$fi")),
            ["find-anything", "find-files"]
        );
        assert_eq!(
            names(complete_draft(&commands, "$mod")),
            Vec::<String>::new()
        );
        let DraftCompletion::Names(candidates) = complete_draft(&commands, "$re") else {
            panic!("a dollar token completes names");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].lane, Lane::Skill);
        assert_eq!(candidates[0].summary, "skill:review summary");
    }

    #[test]
    fn dollar_lane_opens_on_the_last_token_of_a_prompt() {
        let commands = with_skills();
        assert_eq!(
            names(complete_draft(&commands, "check the config with $fi")),
            ["find-anything", "find-files"]
        );
        assert_eq!(
            names(complete_draft(&commands, "line one\nthen $rev")),
            ["review"]
        );
        assert_eq!(
            names(complete_draft(&commands, "$review and then $find-a")),
            ["find-anything"]
        );
        assert_eq!(
            names(complete_draft(&commands, "/skill:review also $find-f")),
            ["find-files"]
        );
    }

    #[test]
    fn dollar_lane_waits_for_a_token_that_starts_with_the_sigil() {
        let commands = with_skills();
        for draft in [
            "price is 5$",
            "cost$fi",
            "$review ",
            "$review\n",
            "echo $fi bar",
        ] {
            assert_eq!(
                names(complete_draft(&commands, draft)),
                Vec::<String>::new(),
                "{draft:?}"
            );
        }
    }

    #[test]
    fn dollar_in_other_slash_commands_stays_with_the_command() {
        let commands = with_skills();
        assert!(matches!(
            complete_draft(&commands, "/model $re"),
            DraftCompletion::Args { name, prefix } if name == "model" && prefix == "$re"
        ));
    }

    #[test]
    fn accepting_a_skill_replaces_only_its_token() {
        let commands = with_skills();
        let pick = |draft: &str| {
            let DraftCompletion::Names(candidates) = complete_draft(&commands, draft) else {
                panic!("expected names for {draft:?}");
            };
            accept(draft, &candidates[0])
        };
        assert_eq!(pick("$re"), "$review ");
        assert_eq!(pick("use $find-a"), "use $find-anything ");
        assert_eq!(pick("one\ntwo $rev"), "one\ntwo $review ");
        assert_eq!(
            pick("/skill:review also $find-f"),
            "/skill:review also $find-files "
        );
        assert_eq!(pick("/mod"), "/model ");
    }
}
