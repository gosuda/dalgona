use super::{
    Deserialize, InputVerdict, RequestParams, STAR_EVENTS, Serialize, ToolCallVerdict, fmt,
};

/// The identity of one of the nine hook events, in [`STAR_EVENTS`] order.
///
/// Serializes as the matching [`STAR_EVENTS`] literal. The Rust-only
/// [`RUST_STREAM_EVENT`] watcher is outside this set: it answers each delta
/// with a [`StreamVerdict`], and no [`HookVerdict`] stands for it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// Observe-only; payload [`SessionStart`].
    SessionStart,
    /// Observe-only; payload [`SessionEnd`].
    SessionEnd,
    /// Guarding; payload [`InputEvent`], verdict [`InputVerdict`].
    Input,
    /// Guarding; payload [`BeforeTurn`], verdict optional added text.
    BeforeTurn,
    /// Guarding; payload [`BeforeRequest`], verdict optional [`RequestParams`].
    BeforeRequest,
    /// Guarding; payload [`ToolCallEvent`], verdict [`ToolCallVerdict`].
    ToolCall,
    /// Observe-only; payload [`ToolResultEvent`].
    ToolResult,
    /// Observe-only; payload [`TurnEnd`].
    TurnEnd,
    /// Observe-only; payload [`Settled`].
    Settled,
}

impl HookEvent {
    /// Every hook event, in the order of [`STAR_EVENTS`].
    pub const ALL: [Self; 9] = [
        Self::SessionStart,
        Self::SessionEnd,
        Self::Input,
        Self::BeforeTurn,
        Self::BeforeRequest,
        Self::ToolCall,
        Self::ToolResult,
        Self::TurnEnd,
        Self::Settled,
    ];

    /// Returns the event's [`STAR_EVENTS`] literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => STAR_EVENTS[0],
            Self::SessionEnd => STAR_EVENTS[1],
            Self::Input => STAR_EVENTS[2],
            Self::BeforeTurn => STAR_EVENTS[3],
            Self::BeforeRequest => STAR_EVENTS[4],
            Self::ToolCall => STAR_EVENTS[5],
            Self::ToolResult => STAR_EVENTS[6],
            Self::TurnEnd => STAR_EVENTS[7],
            Self::Settled => STAR_EVENTS[8],
        }
    }

    /// Whether hooks for this event return a [`HookVerdict`].
    ///
    /// Observe-only events return `()`; no verdict exists for them.
    #[must_use]
    pub const fn is_guarding(self) -> bool {
        matches!(
            self,
            Self::Input | Self::BeforeTurn | Self::BeforeRequest | Self::ToolCall
        )
    }
}

impl fmt::Display for HookEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The final result of one guarding hook chain.
///
/// Each variant answers exactly one guarding [`HookEvent`], given by
/// [`HookVerdict::event`]. Observe-only events have no variant, so no
/// value here can stand for an observer's approval.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HookVerdict {
    /// The `input` result.
    Input(InputVerdict),
    /// The `before_turn` result; `None` adds no text.
    BeforeTurn(Option<Box<str>>),
    /// The `before_request` result; `None` leaves the parameters unchanged.
    BeforeRequest(Option<RequestParams>),
    /// The `tool_call` result.
    ToolCall(ToolCallVerdict),
}

impl HookVerdict {
    /// Returns the one guarding event this verdict answers.
    #[must_use]
    pub const fn event(&self) -> HookEvent {
        match self {
            Self::Input(_) => HookEvent::Input,
            Self::BeforeTurn(_) => HookEvent::BeforeTurn,
            Self::BeforeRequest(_) => HookEvent::BeforeRequest,
            Self::ToolCall(_) => HookEvent::ToolCall,
        }
    }
}

/// A guarding hook verdict checked against the event it answers.
///
/// The only constructor is [`HookOutcome::new`], so holding one proves the
/// pair is valid: the event is guarding and the verdict answers it. An
/// observe-only event can never produce an outcome.
#[derive(Clone, Debug, PartialEq)]
pub struct HookOutcome {
    pub(super) verdict: HookVerdict,
}

impl HookOutcome {
    /// Pairs `verdict` with `event` when the verdict answers that event.
    ///
    /// # Errors
    /// Returns [`HookMismatch::Observer`] when `event` is observe-only and
    /// [`HookMismatch::WrongEvent`] when the verdict answers another event.
    pub fn new(event: HookEvent, verdict: HookVerdict) -> Result<Self, HookMismatch> {
        let answered = verdict.event();
        if answered == event {
            return Ok(Self { verdict });
        }
        if event.is_guarding() {
            return Err(HookMismatch::WrongEvent {
                event,
                verdict: answered,
            });
        }
        Err(HookMismatch::Observer {
            event,
            verdict: answered,
        })
    }

    /// Returns the guarding event this outcome answers.
    #[must_use]
    pub const fn event(&self) -> HookEvent {
        self.verdict.event()
    }

    /// Borrows the checked verdict.
    #[must_use]
    pub const fn verdict(&self) -> &HookVerdict {
        &self.verdict
    }

    /// Returns the checked verdict.
    #[must_use]
    pub fn into_verdict(self) -> HookVerdict {
        self.verdict
    }
}

/// A hook verdict was paired with an event it does not answer.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HookMismatch {
    /// The event is observe-only and has no verdict.
    #[error("observe-only hook event {event} returns no verdict; got the {verdict} verdict")]
    Observer {
        /// The observe-only event.
        event: HookEvent,
        /// The event the supplied verdict answers.
        verdict: HookEvent,
    },
    /// The verdict answers a different guarding event.
    #[error("hook event {event} cannot take the {verdict} verdict")]
    WrongEvent {
        /// The guarding event.
        event: HookEvent,
        /// The event the supplied verdict answers.
        verdict: HookEvent,
    },
}

/// The current admission and completion state of an agent handle.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HandleStatus {
    /// Waiting for capacity admission.
    Pending,
    /// Admitted and currently running.
    Running,
    /// Completed successfully.
    Done,
    /// Failed with an error.
    Failed,
    /// Cancelled before successful completion.
    Cancelled,
}
