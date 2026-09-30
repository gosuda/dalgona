use super::{Deserialize, JobId, PathBuf, Serialize, TurnId, lex::LexError};

/// The session state expected by a prompt command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Expect {
    /// Require the session to be idle.
    #[default]
    Idle,
    /// Require the session to be after the specified turn.
    After(
        /// The turn whose completion is required.
        TurnId,
    ),
}

/// The running object targeted by a cancellation command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelScope {
    /// Cancel a running turn.
    Turn(
        /// The running turn to cancel.
        TurnId,
    ),
    /// Cancel a live job.
    Job(
        /// The job to cancel.
        JobId,
    ),
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(super) enum CancelScopeShape {
    Turn { turn: TurnId },
    Job { job: JobId },
}

impl Serialize for CancelScope {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Turn(turn) => CancelScopeShape::Turn { turn: *turn },
            Self::Job(job) => CancelScopeShape::Job { job: *job },
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CancelScope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match CancelScopeShape::deserialize(deserializer)? {
            CancelScopeShape::Turn { turn } => Self::Turn(turn),
            CancelScopeShape::Job { job } => Self::Job(job),
        })
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for CancelScope {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CancelScope".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <CancelScopeShape as schemars::JsonSchema>::json_schema(generator)
    }
}

/// How widely a settings command applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Save {
    /// Apply to this session only.
    SessionOnly,
    /// Apply to this session and persist as the product default.
    SessionAndDefault,
}

/// The file format selected for a session export.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// Rendered Markdown text.
    Markdown,
    /// The journal codec chain.
    Jsonl,
}

/// The session activity that keeps a command from running.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BusyState {
    /// A turn is running or settling.
    Running,
    /// A compaction is in progress.
    Compacting,
}

/// Why a session import file could not be read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ImportFailure {
    /// The import path does not exist.
    MissingFile {
        /// The absolute path that was missing.
        abs: PathBuf,
    },
    /// The file is not a session export.
    NotSession {
        /// The absolute path that was read.
        abs: PathBuf,
        /// The one-based offending line.
        line: u64,
        /// Why the line is not a session record.
        reason: Box<str>,
    },
    /// A line exceeds the import line cap.
    LineTooLong {
        /// The absolute path that was read.
        abs: PathBuf,
        /// The one-based offending line.
        line: u64,
    },
}

/// A command rejected with a structured, prose-free reason.
///
/// The dispatcher returns these values without rendering text; the
/// built-in commands part owns the only prose renderer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum CommandError {
    /// No built-in or plugin command has this name.
    Unknown {
        /// The name that was resolved.
        name: Box<str>,
        /// The closest registered name within edit distance two.
        suggestion: Option<Box<str>>,
    },
    /// The command needs an idle session.
    Busy {
        /// The canonical resolved command name.
        cmd: Box<str>,
        /// The activity keeping the session busy.
        state: BusyState,
    },
    /// The command received the wrong number of arguments.
    Arity {
        /// The canonical resolved command name.
        cmd: Box<str>,
        /// The raw argument tail that was rejected.
        args: Box<str>,
    },
    /// The argument tail fails the fixed command lexer.
    Lex {
        /// The raw argument tail that was rejected.
        args: Box<str>,
        /// The lexer rejection.
        error: LexError,
    },
    /// A single-instance command already has a running job.
    JobCap {
        /// The canonical resolved command name.
        cmd: Box<str>,
    },
    /// Too many plugin commands are already running.
    PluginCap,
    /// The import file could not be read.
    Import(ImportFailure),
}

/// The finished prose triple front ends render for a command failure.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ErrorTriple {
    /// What failed, as one line.
    pub what: Box<str>,
    /// Why it failed, as one line.
    pub why: Box<str>,
    /// What to do next, as one line.
    pub fix: Box<str>,
}

/// The payload of a completed command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Output {
    /// Completed with no payload.
    Nothing,
    /// Plain text the client renders as-is.
    Text(Box<str>),
    /// Markdown text the client renders.
    Markdown(Box<str>),
    /// A label-value table; each row's first cell holds the row label.
    Table(Vec<Vec<Box<str>>>),
}

/// The picker a command asks the client to show.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Chooser {
    /// Pick a settings row to change.
    Settings,
    /// Pick a model from the cached catalog.
    Model,
    /// Pick a thinking level.
    Thinking,
    /// Multi-select the models `/model` lists.
    ScopedModels,
    /// Pick a provider to sign in to.
    Login,
    /// Pick a provider to sign out of.
    Logout,
    /// Pick a session to open.
    Session,
    /// Pick a session tree point to move to.
    Tree,
    /// Pick a user message to fork from.
    ForkPoint,
}

/// A client-side action carrying no session mutation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum FrontAction {
    /// Copy text to the terminal clipboard.
    CopyReply {
        /// The text to copy.
        text: Box<str>,
    },
    /// Import a session file the handler already validated.
    Import {
        /// The absolute import path.
        path: PathBuf,
    },
    /// Run the credential flow for a provider.
    Login {
        /// The provider id.
        provider: Box<str>,
    },
    /// Remove the credential flow for a provider.
    Logout {
        /// The provider id.
        provider: Box<str>,
    },
    /// Open a new session.
    NewSession,
    /// Switch to a resolved session.
    Resume {
        /// The resolved session.
        session: crate::view::SessionSummary,
    },
    /// Quit the product.
    Quit,
    /// Show keyboard shortcuts.
    ShowKeys,
}

/// One argument completion item offered for a command tail.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    /// The text inserted into the composer.
    pub value: Box<str>,
    /// The short display label.
    pub label: Box<str>,
    /// A longer description shown beside the label.
    pub detail: Box<str>,
}
