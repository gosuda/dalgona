use super::{
    ApprovalMode, CancelScope, EntryId, Expect, ExportFormat, Mode, ModelRoute, Part, PathBuf,
    Save, Serialize, ThinkingLevel, TurnId,
};

/// A command submitted to a session actor.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Start a prompt when the session matches the expected state.
    Prompt {
        /// The session state required before accepting the prompt.
        expect: Expect,
        /// The user content for the prompt.
        content: Vec<Part>,
    },
    /// Add input to the currently running turn.
    Steer {
        /// The turn that receives the input.
        turn: TurnId,
        /// The content to queue for the next request.
        content: Vec<Part>,
    },
    /// Queue input to run after the named turn ends.
    FollowUp {
        /// The turn that must still be running when the follow-up is accepted.
        turn: TurnId,
        /// The content to queue for the next turn.
        content: Vec<Part>,
    },
    /// Cancel a running turn or job.
    Cancel {
        /// The turn or job to cancel.
        scope: CancelScope,
    },
    /// Select the model route for subsequent requests.
    SetModel {
        /// The model route to use.
        model: ModelRoute,
        /// Whether to persist the route as the product default.
        save: Save,
    },
    /// Select the reasoning intensity for subsequent requests.
    SetThinking {
        /// The requested reasoning intensity.
        level: ThinkingLevel,
        /// Whether to persist the level as the product default.
        save: Save,
    },
    /// Select the approval policy for subsequent tool calls.
    SetApproval {
        /// The approval policy to use.
        mode: ApprovalMode,
        /// Whether to persist the policy as the product default.
        save: Save,
    },
    /// Select the harness mode for subsequent turns.
    SetMode {
        /// The harness mode to use.
        mode: Mode,
        /// Whether to persist the mode as the product default.
        save: Save,
    },
    /// Start context compaction while the session is idle.
    Compact {
        /// Optional focus text for the compaction request.
        focus: Option<Box<str>>,
    },
    /// Move the session's active leaf to an existing entry.
    MoveLeaf(
        /// The entry to make the active leaf.
        EntryId,
    ),
    /// Create a child session rooted at an existing entry.
    Fork(
        /// The entry at which to fork.
        EntryId,
    ),
    /// Clone the current session into a new session.
    Clone,
    /// Set the session's display name.
    Rename(
        /// The new session name.
        Box<str>,
    ),
    /// Run a named extension operation.
    Run {
        /// The registered operation name.
        name: Box<str>,
        /// The operation's argument text.
        args: Box<str>,
        /// The session state required before running, or none for any state.
        expected: Option<Expect>,
    },
    /// Replace the model list shown by `/model`.
    SetScopedModels(
        /// The scoped model ids; empty clears the scope.
        Vec<Box<str>>,
    ),
    /// Export the session to a file.
    Export {
        /// The export target; relative paths resolve against the workspace.
        path: Option<PathBuf>,
        /// The export file format.
        format: ExportFormat,
    },
    /// Reload user plugins.
    ReloadPlugins,
}

#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(super) enum CommandShape<'a> {
    Prompt {
        expect: &'a Expect,
        content: &'a [Part],
    },
    Steer {
        turn: &'a TurnId,
        content: &'a [Part],
    },
    FollowUp {
        turn: &'a TurnId,
        content: &'a [Part],
    },
    Cancel {
        scope: &'a CancelScope,
    },
    SetModel {
        model: &'a ModelRoute,
        save: &'a Save,
    },
    SetThinking {
        level: &'a ThinkingLevel,
        save: &'a Save,
    },
    SetApproval {
        mode: &'a ApprovalMode,
        save: &'a Save,
    },
    SetMode {
        mode: &'a Mode,
        save: &'a Save,
    },
    Compact {
        focus: Option<&'a str>,
    },
    MoveLeaf {
        entry: &'a EntryId,
    },
    Fork {
        entry: &'a EntryId,
    },
    Clone,
    Rename {
        name: &'a str,
    },
    Run {
        name: &'a str,
        args: &'a str,
        expected: Option<&'a Expect>,
    },
    SetScopedModels {
        scoped_models: &'a [Box<str>],
    },
    Export {
        path: Option<&'a PathBuf>,
        format: &'a ExportFormat,
    },
    ReloadPlugins,
}

// The schema feature reads this mirror enum's serde attributes;
// runtime decode goes through the tagged carrier because `SetModel`
// carries a `ModelRoute`, which needs its raw member text.
#[expect(
    dead_code,
    reason = "schemars reads the variants' serde attributes; nothing constructs them"
)]
#[derive(Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(super) enum CommandFields {
    Prompt {
        #[serde(default)]
        expect: Expect,
        content: Vec<Part>,
    },
    Steer {
        turn: TurnId,
        content: Vec<Part>,
    },
    FollowUp {
        turn: TurnId,
        content: Vec<Part>,
    },
    Cancel {
        scope: CancelScope,
    },
    SetModel {
        model: ModelRoute,
        save: Save,
    },
    SetThinking {
        level: ThinkingLevel,
        save: Save,
    },
    SetApproval {
        mode: ApprovalMode,
        save: Save,
    },
    SetMode {
        mode: Mode,
        save: Save,
    },
    Compact {
        focus: Option<Box<str>>,
    },
    MoveLeaf {
        entry: EntryId,
    },
    Fork {
        entry: EntryId,
    },
    Clone,
    Rename {
        name: Box<str>,
    },
    Run {
        name: Box<str>,
        args: Box<str>,
        expected: Option<Expect>,
    },
    SetScopedModels {
        scoped_models: Vec<Box<str>>,
    },
    Export {
        path: Option<PathBuf>,
        format: ExportFormat,
    },
    ReloadPlugins,
}

impl Serialize for Command {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let shape = match self {
            Self::Prompt { expect, content } => CommandShape::Prompt { expect, content },
            Self::Steer { turn, content } => CommandShape::Steer { turn, content },
            Self::FollowUp { turn, content } => CommandShape::FollowUp { turn, content },
            Self::Cancel { scope } => CommandShape::Cancel { scope },
            Self::SetModel { model, save } => CommandShape::SetModel { model, save },
            Self::SetThinking { level, save } => CommandShape::SetThinking { level, save },
            Self::SetApproval { mode, save } => CommandShape::SetApproval { mode, save },
            Self::SetMode { mode, save } => CommandShape::SetMode { mode, save },
            Self::Compact { focus } => CommandShape::Compact {
                focus: focus.as_deref(),
            },
            Self::MoveLeaf(entry) => CommandShape::MoveLeaf { entry },
            Self::Fork(entry) => CommandShape::Fork { entry },
            Self::Clone => CommandShape::Clone,
            Self::Rename(name) => CommandShape::Rename { name },
            Self::Run {
                name,
                args,
                expected,
            } => CommandShape::Run {
                name,
                args,
                expected: expected.as_ref(),
            },
            Self::SetScopedModels(ids) => CommandShape::SetScopedModels { scoped_models: ids },
            Self::Export { path, format } => CommandShape::Export {
                path: path.as_ref(),
                format,
            },
            Self::ReloadPlugins => CommandShape::ReloadPlugins,
        };
        shape.serialize(serializer)
    }
}
