//! Completion popup over `Host::commands` entries and `@` files.

use dal_core::CommandSpec;

use crate::composer::slash_completion;

/// One completion candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Command name without the leading slash.
    pub name: String,
    /// Short summary.
    pub summary: String,
}

/// Filters command specs by a `/` prefix; empty command lists render nothing.
#[must_use]
pub fn complete_commands(commands: &[CommandSpec], prefix: &str) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = commands
        .iter()
        .filter(|spec| spec.name.as_str().starts_with(prefix))
        .filter(|spec| !spec.name.as_str().starts_with("skill:"))
        .map(|spec| Candidate {
            name: spec.name.as_str().to_owned(),
            summary: spec.summary.to_string(),
        })
        .collect();
    candidates.sort_by(|left, right| left.name.cmp(&right.name));
    candidates
}

/// Filters skill specs by a `$` prefix with the same match and ranking.
#[must_use]
pub fn complete_skills(commands: &[CommandSpec], prefix: &str) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = commands
        .iter()
        .filter(|spec| spec.name.as_str().starts_with(prefix))
        .map(|spec| Candidate {
            name: spec.name.as_str().to_owned(),
            summary: spec.summary.to_string(),
        })
        .collect();
    candidates.sort_by(|left, right| left.name.cmp(&right.name));
    candidates
}
/// What a full composer draft completes to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DraftCompletion {
    /// Complete the command name from the registry.
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
/// Drafts still on the command name filter the registry, including
/// `plugin:command` names; drafts with argument text return the lexed name
/// and partial argument for host-side value completion. Plain text drafts
/// complete nothing.
#[must_use]
pub fn complete_draft(commands: &[CommandSpec], draft: &str) -> DraftCompletion {
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

#[cfg(test)]
mod tests {
    use dal_core::{CommandName, CommandSpec};

    use super::{DraftCompletion, complete_commands, complete_draft};

    fn registry() -> Vec<CommandSpec> {
        ["model", "quality:todos"]
            .into_iter()
            .map(|name| CommandSpec {
                name: CommandName::parse(name).unwrap(),
                summary: "test command".into(),
                args_hint: None,
            })
            .collect()
    }

    #[test]
    fn empty_command_list_renders_nothing() {
        assert_eq!(complete_commands(&[], "mod"), []);
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
}
