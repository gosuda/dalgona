use super::{
    CallId, Caps, Deserialize, Deserializer, EntryId, ModelInfo, Name, Part, RawJson,
    RegistrationError, RequestParams, Serialize, SessionId, Stop, Tagged, ToolClass, TurnId,
    Workspace, de,
};

/// A rule's interrupt behavior when a matching condition is found.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum InterruptMode {
    /// Interrupt for any matching content.
    #[serde(rename = "always")]
    Always,
    /// Interrupt only for prose content.
    #[serde(rename = "prose-only")]
    ProseOnly,
    /// Interrupt only for tool arguments.
    #[serde(rename = "tool-only")]
    ToolOnly,
    /// Never interrupt for this rule.
    #[serde(rename = "never")]
    Never,
}

/// A rule's repeated-match policy.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    /// Apply only once.
    Once,
    /// Apply again after its configured gap.
    #[serde(rename = "after-gap")]
    AfterGap,
}

/// Selects stream content classes and optional tool names for a rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    /// Whether assistant text is included.
    pub text: bool,
    /// Whether reasoning text is included.
    pub thinking: bool,
    /// Whether tool arguments are included.
    pub tool: bool,
    /// Optional restriction to named tools.
    #[serde(deserialize_with = "super::names::deserialize_tool_names")]
    pub named_tools: Vec<Name>,
}

/// A rule declaration whose patterns are checked when a rules set is built.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RuleRecord {
    /// The rule's checked identifier.
    pub name: Name,
    /// One to sixteen raw regular-expression sources.
    pub patterns: Vec<Box<str>>,
    /// The reminder or instruction text to inject.
    pub text: Box<str>,
    /// An optional judge instruction.
    pub judge: Option<Box<str>>,
    /// Optional content selection; absence preserves the rules default.
    pub scope: Option<Scope>,
    /// Optional path globs that select matching files.
    pub globs: Option<Vec<Box<str>>>,
    /// Optional agent names that select this rule.
    pub agents: Option<Vec<Box<str>>>,
    /// Optional interruption behavior.
    pub mode: Option<InterruptMode>,
    /// Optional repeated-match behavior.
    pub repeat_mode: Option<RepeatMode>,
    /// Optional number of deltas between repeated applications.
    pub repeat_gap: Option<u16>,
    /// Whether the rule applies without a match report.
    pub always_apply: bool,
    /// Whether matches are reported.
    pub report: bool,
    /// Whether the rule is active.
    pub enabled: bool,
}

impl RuleRecord {
    /// Validates the declaration's pattern count and repeat gap.
    ///
    /// Pattern text is intentionally not compiled here; rule-set construction
    /// owns matcher limits and malformed-pattern handling.
    ///
    /// # Errors
    /// Returns [`RegistrationError::InvalidRule`] for an invalid pattern count
    /// or repeat gap.
    pub fn validate(&self) -> Result<(), RegistrationError> {
        let unconditional = self.patterns.is_empty() && self.always_apply;
        if !unconditional && !(1..=16).contains(&self.patterns.len()) {
            return Err(RegistrationError::InvalidRule {
                name: self.name.clone(),
                reason: "must declare one to sixteen patterns, or none with always_apply".into(),
            });
        }
        if self
            .repeat_gap
            .is_some_and(|gap| !(1..=1000).contains(&gap))
        {
            return Err(RegistrationError::InvalidRule {
                name: self.name.clone(),
                reason: "repeat_gap must be within 1..=1000".into(),
            });
        }
        Ok(())
    }
}

/// A stream channel used by Rust stream watchers.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Channel {
    /// Assistant-visible text.
    Text,
    /// Reasoning text.
    Thinking,
    /// Arguments for a named tool.
    ToolArgs {
        /// The tool whose arguments are streamed.
        #[serde(deserialize_with = "super::names::deserialize_tool_name")]
        tool: Name,
    },
}

/// A stream watcher's decision for the current delta.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum StreamVerdict {
    /// Keep processing without injecting a reminder.
    Continue,
    /// Interrupt with a named rule and reminder text.
    Interrupt {
        /// The rule that matched.
        rule: Box<str>,
        /// The reminder to inject.
        inject: Box<str>,
    },
}
/// Action taken for one stream-rule match.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StreamFireAction {
    /// Interrupt the provider response and retry.
    Interrupt,
    /// Continue streaming and persist a system reminder.
    Reminder,
    /// Continue streaming without a system reminder.
    Report,
}

/// One newly observed stream-rule fire.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StreamFire {
    /// Stable index used to create and commit this fire's durable record.
    pub index: usize,
    /// The matched rule name.
    pub rule: Box<str>,
    /// The action selected by the rule.
    pub action: StreamFireAction,
    /// Reminder or interrupt text, absent for reports.
    pub text: Option<Box<str>>,
    /// Whether a judge verdict gates delivery.
    pub judged: bool,
}

/// The interrupt budget for one response attempt.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WatchBudget {
    /// Interrupt-admitting rules may stop the stream.
    Interrupts,
    /// Every match degrades to a reminder or a report.
    RemindersOnly,
}

/// One stream-rule fire visible on the current journal leaf.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct GateSeed {
    /// The rule whose repeat gate is restored; TTSR names exceed the `Name` grammar.
    pub rule: Box<str>,
    /// The turn that fired the rule.
    pub turn: TurnId,
    /// The reminder entry that carried the fire.
    pub entry: EntryId,
}

/// Information available when a session starts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionStart {
    /// The session that started.
    pub session: SessionId,
    /// The session's workspace.
    pub workspace: Workspace,
    /// Whether this session resumed prior state.
    pub resumed: bool,
}

/// Information available when a session ends.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SessionEnd {
    /// The session that ended.
    pub session: SessionId,
    /// Why the session ended.
    pub reason: Box<str>,
}

/// Input text and content supplied to input hooks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct InputEvent {
    /// The input content, including any image or blob parts.
    pub content: Vec<Part>,
}

/// Context supplied to a before-turn hook.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BeforeTurn {
    /// The active turn.
    pub turn: TurnId,
    /// The prompt text for this turn.
    pub text: Box<str>,
}

/// Context supplied before a model request is made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BeforeRequest {
    /// The active turn.
    pub turn: TurnId,
    /// The request round within the turn.
    pub round: u32,
    /// The resolved model metadata.
    pub model: ModelInfo,
    /// The model's supported capabilities.
    pub caps: Caps,
    /// The current request tuning parameters.
    pub params: RequestParams,
    /// Whether the user explicitly selected the current thinking level.
    pub thinking_explicit: bool,
}

/// Context supplied before a tool call is dispatched.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ToolCallEvent {
    /// The active turn.
    pub turn: TurnId,
    /// The provider's call identity.
    pub call: CallId,
    /// The checked tool name.
    #[serde(deserialize_with = "super::names::deserialize_tool_name")]
    pub tool: Name,
    /// The final class computed from the call arguments.
    pub class: ToolClass,
    /// Raw tool arguments, preserved without re-encoding.
    pub args: RawJson,
}

/// The outcome information made available after a tool call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ToolResultEvent {
    /// The active turn.
    pub turn: TurnId,
    /// The provider's call identity.
    pub call: CallId,
    /// The checked tool name.
    #[serde(deserialize_with = "super::names::deserialize_tool_name")]
    pub tool: Name,
    /// Whether the tool call succeeded.
    pub ok: bool,
    /// A bounded human-readable result preview.
    pub preview: Box<str>,
}

/// The terminal result of a turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TurnEnd {
    /// The completed turn.
    pub turn: TurnId,
    /// Why the turn stopped.
    pub stop: Stop,
}

/// A notification that a turn has settled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Settled {
    /// The settled turn.
    pub turn: TurnId,
    /// The assistant-visible reply text from this turn.
    pub reply_text: Box<str>,
}

/// The typed result of an input hook.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum InputVerdict {
    /// Continue dispatch without replacing input content.
    Continue,
    /// Replace input content with the supplied parts.
    Transform(Vec<Part>),
    /// Mark the input handled and stop input dispatch.
    Handled,
}

/// The typed result of a tool-call hook.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ToolCallVerdict {
    /// Allow the tool call with its original arguments.
    Allow,
    /// Block the tool call with a reason.
    Block {
        /// Why the tool call was blocked.
        reason: Box<str>,
    },
    /// Replace only the call arguments.
    Rewrite {
        /// The replacement arguments, preserved as raw JSON.
        args: RawJson,
    },
}

// Verdict and job ops decode through the raw tagged carrier: their
// `RawJson` members cannot pass through serde's internally tagged
// content buffering, which cannot hand raw bytes to a nested member.
#[derive(Deserialize)]
pub(super) struct BlockVerdictFields {
    pub(super) reason: Box<str>,
}

#[derive(Deserialize)]
pub(super) struct RewriteVerdictFields {
    pub(super) args: RawJson,
}

impl<'de> Deserialize<'de> for ToolCallVerdict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let tagged = Tagged::decode(deserializer, "type", &["allow", "block", "rewrite"])?;
        match tagged.kind() {
            "allow" => Ok(Self::Allow),
            "block" => {
                let wire: BlockVerdictFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Block {
                    reason: wire.reason,
                })
            }
            "rewrite" => {
                let wire: RewriteVerdictFields =
                    sonic_rs::from_str(tagged.raw()).map_err(de::Error::custom)?;
                Ok(Self::Rewrite { args: wire.args })
            }
            other => Err(de::Error::custom(format!("unknown verdict type `{other}`"))),
        }
    }
}

/// Names registered for Starlark hook dispatch.
pub const STAR_EVENTS: [&str; 9] = [
    "session_start",
    "session_end",
    "input",
    "before_turn",
    "before_request",
    "tool_call",
    "tool_result",
    "turn_end",
    "settled",
];

/// The event name reserved for Rust-only stream watchers.
pub const RUST_STREAM_EVENT: &str = "output_stream";
