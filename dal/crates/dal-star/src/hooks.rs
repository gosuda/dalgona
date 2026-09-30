//! Plugin lifecycle hooks as host hook values (§P05).
//!
//! Each call builds one immutable event value: the P05 payload fields read
//! as attributes, and the verdict methods of that event kind. A verdict is
//! bound to the event that minted it, so a dictionary, `None`, or a verdict
//! from another event cannot stand in. The host dispatch chain owns the
//! per-event failure policy; this module reports one verdict or one
//! [`HookError`].

#![expect(unsafe_code, reason = "starlark value derives")]

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dal_agent::ext::{BoxFuture, Hook, HookCx, HookError, ObserveHook};
use dal_core::ext::{
    BeforeRequest, BeforeTurn, InputEvent, InputVerdict, Phase, SessionEnd, SessionStart,
    ToolCallEvent, ToolCallVerdict, ToolResultEvent, TurnEnd,
};
use dal_core::{HookEvent, Part, RawJson, RequestParams, ThinkingLevel};
use starlark::any::ProvidesStaticType;
use starlark::environment::{Methods, MethodsBuilder};
use starlark::starlark_module;
use starlark::values::{Heap, NoSerialize, StarlarkValue, Value, ValueLike};

use crate::error::api_error;
use crate::invoke::{self, Arg, Handler, InvokeFailure, InvokeOutput};
use crate::validate::LoadedPlugin;
use crate::value::{self, CodecError};

/// The byte bound on a `tool_result` preview (§P05).
const MAX_PREVIEW: usize = 4 << 10;

/// Issues event identities; unique for the life of the process.
static NEXT_EVENT: AtomicU64 = AtomicU64::new(1);

/// One hook event: its kind, identity, and payload object.
#[derive(Clone, Debug)]
pub(crate) struct Event {
    /// The subscribed event kind.
    kind: HookEvent,
    /// The identity a verdict must carry to answer this event.
    id: u64,
    /// The P05 payload fields as one transport object.
    payload: value::Value,
}

impl Event {
    /// Builds an event of `kind` with a fresh identity.
    fn new(kind: HookEvent, payload: value::Value) -> Self {
        Self {
            kind,
            id: NEXT_EVENT.fetch_add(1, Ordering::SeqCst),
            payload,
        }
    }

    /// Looks up one payload field.
    fn field(&self, name: &str) -> Option<&value::Value> {
        let value::Value::Object(fields) = &self.payload else {
            return None;
        };
        fields
            .iter()
            .find(|(key, _)| key.as_ref() == name)
            .map(|(_, item)| item)
    }
}

/// The `event` value a hook handler receives (§P05).
///
/// Payload fields materialize on access; they were decoded under the §R10
/// bounds, so materialization cannot fail.
#[derive(ProvidesStaticType, NoSerialize, allocative::Allocative)]
#[repr(C)]
pub(crate) struct EventValue {
    /// The event this value answers for.
    #[allocative(skip)]
    event: Event,
}

starlark::starlark_simple_value!(EventValue);

impl EventValue {
    /// Allocates the value for `event`.
    pub(crate) fn alloc(heap: Heap<'_>, event: Event) -> Value<'_> {
        heap.alloc(Self { event })
    }
}

impl fmt::Debug for EventValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for EventValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "event({})", self.event.kind.as_str())
    }
}

#[starlark::values::starlark_value(type = "event")]
impl<'v> StarlarkValue<'v> for EventValue {
    #[expect(
        clippy::expect_used,
        reason = "event payloads are decoded under the §R10 bounds"
    )]
    fn get_attr(&self, attribute: &str, heap: Heap<'v>) -> Option<Value<'v>> {
        let field = self.event.field(attribute)?.clone();
        Some(
            field
                .into_starlark(heap)
                .expect("event payloads are decoded under the §R10 bounds"),
        )
    }

    fn has_attr(&self, attribute: &str, _heap: Heap<'v>) -> bool {
        self.event.field(attribute).is_some()
    }

    fn dir_attr(&self) -> Vec<String> {
        let value::Value::Object(fields) = &self.event.payload else {
            return Vec::new();
        };
        fields.iter().map(|(key, _)| key.to_string()).collect()
    }

    fn get_methods() -> Option<&'static Methods> {
        Some(EVENT_METHODS.methods())
    }
}

starlark::methods_static!(EVENT_METHODS = event_methods);

/// One typed hook verdict (§P05).
#[derive(Clone, Debug)]
pub(crate) enum Verdict {
    /// `tool_call`: run the call as requested.
    Allow,
    /// `tool_call`: refuse the call with a reason.
    Block(Box<str>),
    /// `tool_call`: replace the call arguments.
    Rewrite(RawJson),
    /// `input`, `before_turn`, `before_request`: no change.
    Continue,
    /// `input`: replace the text.
    Transform(Box<str>),
    /// `input`: the hook consumed the input.
    Handled,
    /// `before_turn`: add a text block.
    Append(Box<str>),
    /// `before_request`: override request parameters.
    Params(ParamsPatch),
}

/// Request parameter overrides from `event.params(...)`; absent fields keep
/// the incoming value.
#[derive(Clone, Debug, Default)]
pub(crate) struct ParamsPatch {
    /// The reasoning level.
    thinking: Option<ThinkingLevel>,
    /// The provider effort label.
    effort: Option<Box<str>>,
    /// The sampling temperature.
    temperature: Option<f64>,
    /// The output token cap.
    max_output_tokens: Option<u32>,
}

impl ParamsPatch {
    /// Applies the overrides to `params`.
    fn apply(self, params: RequestParams) -> RequestParams {
        RequestParams {
            thinking: self.thinking.unwrap_or(params.thinking),
            effort: self.effort.or(params.effort),
            temperature: self.temperature.or(params.temperature),
            max_output_tokens: self.max_output_tokens.or(params.max_output_tokens),
        }
    }
}

/// A verdict together with the event identity that minted it.
#[derive(Clone, Debug)]
pub(crate) struct Minted {
    /// The minting event.
    event: u64,
    /// The verdict.
    verdict: Verdict,
}

/// A returned verdict value; only event methods construct one.
#[derive(ProvidesStaticType, NoSerialize, allocative::Allocative)]
#[repr(C)]
pub(crate) struct VerdictValue {
    /// The verdict and its minting event.
    #[allocative(skip)]
    minted: Minted,
}

starlark::starlark_simple_value!(VerdictValue);

impl fmt::Debug for VerdictValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for VerdictValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("verdict")
    }
}

#[starlark::values::starlark_value(type = "verdict")]
#[expect(
    clippy::elidable_lifetime_names,
    reason = "StarlarkValue requires implementations for every value lifetime"
)]
impl<'v> StarlarkValue<'v> for VerdictValue {}

/// The minted verdict a returned value carries, if it is a verdict.
pub(crate) fn minted_of(value: Value<'_>) -> Option<Minted> {
    value
        .downcast_ref::<VerdictValue>()
        .map(|verdict| verdict.minted.clone())
}

/// Mints `verdict` for the event behind `this` when its kind is in `kinds`.
fn mint(
    this: Value<'_>,
    kinds: &[HookEvent],
    name: &str,
    verdict: Verdict,
) -> starlark::Result<VerdictValue> {
    let event = &this
        .downcast_ref::<EventValue>()
        .ok_or_else(|| api_error(format!("{name}: receiver is not a hook event")))?
        .event;
    if !kinds.contains(&event.kind) {
        return Err(api_error(format!(
            "{name}: not a verdict of the `{}` event",
            event.kind.as_str()
        )));
    }
    Ok(VerdictValue {
        minted: Minted {
            event: event.id,
            verdict,
        },
    })
}

/// The events whose chain can continue unchanged.
const CONTINUING: [HookEvent; 3] = [
    HookEvent::Input,
    HookEvent::BeforeTurn,
    HookEvent::BeforeRequest,
];

#[starlark_module]
fn event_methods(builder: &mut MethodsBuilder) {
    /// `tool_call`: run the call as requested.
    fn allow<'v>(#[starlark(this)] this: Value<'v>) -> starlark::Result<VerdictValue> {
        mint(this, &[HookEvent::ToolCall], "allow", Verdict::Allow)
    }

    /// `tool_call`: refuse the call with `reason`.
    fn block<'v>(
        #[starlark(this)] this: Value<'v>,
        reason: String,
    ) -> starlark::Result<VerdictValue> {
        mint(
            this,
            &[HookEvent::ToolCall],
            "block",
            Verdict::Block(reason.into()),
        )
    }

    /// `tool_call`: replace the arguments with `args`.
    fn rewrite<'v>(
        #[starlark(this)] this: Value<'v>,
        args: Value<'v>,
    ) -> starlark::Result<VerdictValue> {
        let json = value::Value::from_starlark(args)
            .map_err(|error| api_error(format!("rewrite: {error}")))?
            .to_json();
        let args = RawJson::parse(&json).map_err(|error| api_error(format!("rewrite: {error}")))?;
        mint(
            this,
            &[HookEvent::ToolCall],
            "rewrite",
            Verdict::Rewrite(args),
        )
    }

    /// `input`, `before_turn`, `before_request`: no change.
    fn continue_<'v>(#[starlark(this)] this: Value<'v>) -> starlark::Result<VerdictValue> {
        mint(this, &CONTINUING, "continue_", Verdict::Continue)
    }

    /// `input`: replace the text with `text`.
    fn transform<'v>(
        #[starlark(this)] this: Value<'v>,
        text: String,
    ) -> starlark::Result<VerdictValue> {
        mint(
            this,
            &[HookEvent::Input],
            "transform",
            Verdict::Transform(text.into()),
        )
    }

    /// `input`: the hook consumed the input; later hooks do not run.
    fn handled<'v>(#[starlark(this)] this: Value<'v>) -> starlark::Result<VerdictValue> {
        mint(this, &[HookEvent::Input], "handled", Verdict::Handled)
    }

    /// `before_turn`: add `text` to the turn.
    fn append<'v>(
        #[starlark(this)] this: Value<'v>,
        text: String,
    ) -> starlark::Result<VerdictValue> {
        mint(
            this,
            &[HookEvent::BeforeTurn],
            "append",
            Verdict::Append(text.into()),
        )
    }

    /// `before_request`: override request parameters; the host clamps them
    /// to the model caps.
    fn params<'v>(
        #[starlark(this)] this: Value<'v>,
        #[starlark(require = named)] thinking: Option<String>,
        #[starlark(require = named)] effort: Option<String>,
        #[starlark(require = named)] temperature: Option<starlark::values::float::StarlarkFloat>,
        #[starlark(require = named)] max_output_tokens: Option<i32>,
    ) -> starlark::Result<VerdictValue> {
        let patch = ParamsPatch {
            thinking: thinking.as_deref().map(thinking_level).transpose()?,
            effort: effort.map(Box::from),
            temperature: temperature.map(|t| finite_temperature(t.0)).transpose()?,
            max_output_tokens: max_output_tokens.map(token_cap).transpose()?,
        };
        mint(
            this,
            &[HookEvent::BeforeRequest],
            "params",
            Verdict::Params(patch),
        )
    }
}

/// Parses a reasoning level by its wire spelling.
fn thinking_level(text: &str) -> starlark::Result<ThinkingLevel> {
    let quoted =
        sonic_rs::to_string(text).map_err(|error| api_error(format!("params: {error}")))?;
    sonic_rs::from_str(&quoted)
        .map_err(|_| api_error(format!("params: unknown thinking level `{text}`")))
}

/// Accepts a finite temperature.
fn finite_temperature(temperature: f64) -> starlark::Result<f64> {
    if temperature.is_finite() {
        Ok(temperature)
    } else {
        Err(api_error("params: temperature must be finite"))
    }
}

/// Accepts a nonnegative token cap.
fn token_cap(cap: i32) -> starlark::Result<u32> {
    u32::try_from(cap).map_err(|_| api_error("params: max_output_tokens must not be negative"))
}

/// One plugin hook registered on the host chain.
#[derive(Clone)]
pub(crate) struct ScriptHook {
    /// The owning plugin; the hook lives at `hook` in its table.
    plugin: Arc<LoadedPlugin>,
    /// The index into `plugin.hooks`.
    hook: usize,
}

impl ScriptHook {
    /// Builds the adapter for `plugin.hooks[hook]`.
    pub(crate) fn new(plugin: Arc<LoadedPlugin>, hook: usize) -> Self {
        Self { plugin, hook }
    }

    /// Runs the handler on `event` and returns its settled output.
    async fn run(&self, event: Event, cx: HookCx) -> Result<InvokeOutput, HookError> {
        let decl = &self.plugin.hooks[self.hook];
        let script = cx
            .script
            .as_ref()
            .ok_or_else(|| failed(invoke::NO_SCRIPT_HOST))?;
        let handler = Handler {
            plugin: &self.plugin,
            id: &decl.id,
            phase: Phase::Hook(decl.event),
            run: decl.run,
            cap: cx
                .deadline
                .saturating_duration_since(tokio::time::Instant::now()),
        };
        let entered = invoke::enter(script, handler, Arg::Event(event)).await;
        invoke::settle(entered, None, &decl.id).map_err(hook_error)
    }
}

/// A guarding event: the handler must return one verdict of its kind.
pub(crate) trait GuardEvent: Clone + Send + 'static {
    /// The event kind.
    const KIND: HookEvent;
    /// The typed host verdict.
    type Out: Send + 'static;
    /// Builds the P05 payload object.
    fn payload(&self) -> Result<value::Value, CodecError>;
    /// Converts a verdict of this kind into the host verdict.
    fn decide(self, verdict: Verdict) -> Result<Self::Out, HookError>;
}

/// An observed event: the handler returns `None`.
pub(crate) trait ObservedEvent: Send + 'static {
    /// The event kind.
    const KIND: HookEvent;
    /// Builds the P05 payload object.
    fn payload(&self) -> Result<value::Value, CodecError>;
}

impl<I: GuardEvent> Hook<I, I::Out> for ScriptHook {
    fn call(&self, input: I, cx: HookCx) -> BoxFuture<'static, Result<I::Out, HookError>> {
        let hook = self.clone();
        Box::pin(async move {
            let event = Event::new(I::KIND, input.payload().map_err(|e| failed(e.to_string()))?);
            let id = event.id;
            let output = hook.run(event, cx).await?;
            let minted = output.verdict.ok_or_else(|| {
                failed(format!(
                    "{} hooks must return an event verdict",
                    I::KIND.as_str()
                ))
            })?;
            if minted.event != id {
                return Err(failed("the returned verdict belongs to another event"));
            }
            input.decide(minted.verdict)
        })
    }
}

impl<I: ObservedEvent> ObserveHook<I> for ScriptHook {
    fn call(&self, input: I, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let hook = self.clone();
        Box::pin(async move {
            let event = Event::new(I::KIND, input.payload().map_err(|e| failed(e.to_string()))?);
            let output = hook.run(event, cx).await?;
            match (output.verdict, output.value) {
                (None, value::Value::Null) => Ok(()),
                _ => Err(failed(format!("{} hooks return None", I::KIND.as_str()))),
            }
        })
    }
}

impl GuardEvent for ToolCallEvent {
    const KIND: HookEvent = HookEvent::ToolCall;
    type Out = ToolCallVerdict;

    fn payload(&self) -> Result<value::Value, CodecError> {
        Ok(value::Value::object([
            ("turn", text(self.turn)),
            ("call", text(self.call.as_str())),
            ("tool", text(self.tool.as_str())),
            ("args", value::Value::decode(self.args.as_str())?),
        ]))
    }

    fn decide(self, verdict: Verdict) -> Result<ToolCallVerdict, HookError> {
        match verdict {
            Verdict::Allow => Ok(ToolCallVerdict::Allow),
            Verdict::Block(reason) => Ok(ToolCallVerdict::Block { reason }),
            Verdict::Rewrite(args) => Ok(ToolCallVerdict::Rewrite { args }),
            _ => Err(misfit(Self::KIND)),
        }
    }
}

impl GuardEvent for InputEvent {
    const KIND: HookEvent = HookEvent::Input;
    type Out = InputVerdict;

    fn payload(&self) -> Result<value::Value, CodecError> {
        Ok(value::Value::object([(
            "text",
            text(input_text(&self.content)),
        )]))
    }

    fn decide(self, verdict: Verdict) -> Result<InputVerdict, HookError> {
        match verdict {
            Verdict::Continue => Ok(InputVerdict::Continue),
            Verdict::Transform(text) => {
                Ok(InputVerdict::Transform(replace_text(self.content, text)))
            }
            Verdict::Handled => Ok(InputVerdict::Handled),
            _ => Err(misfit(Self::KIND)),
        }
    }
}

impl GuardEvent for BeforeTurn {
    const KIND: HookEvent = HookEvent::BeforeTurn;
    type Out = Option<String>;

    fn payload(&self) -> Result<value::Value, CodecError> {
        Ok(value::Value::object([
            ("turn", text(self.turn)),
            ("text", text(&self.text)),
        ]))
    }

    fn decide(self, verdict: Verdict) -> Result<Option<String>, HookError> {
        match verdict {
            Verdict::Continue => Ok(None),
            Verdict::Append(text) => Ok(Some(text.into_string())),
            _ => Err(misfit(Self::KIND)),
        }
    }
}

impl GuardEvent for BeforeRequest {
    const KIND: HookEvent = HookEvent::BeforeRequest;
    type Out = Option<RequestParams>;

    fn payload(&self) -> Result<value::Value, CodecError> {
        Ok(value::Value::object([
            ("turn", text(self.turn)),
            ("round", value::Value::Int(i64::from(self.round))),
            ("model", serialized(&self.model)?),
            ("caps", serialized(&self.caps)?),
            ("params", serialized(&self.params)?),
        ]))
    }

    fn decide(self, verdict: Verdict) -> Result<Option<RequestParams>, HookError> {
        match verdict {
            Verdict::Continue => Ok(None),
            Verdict::Params(patch) => Ok(Some(patch.apply(self.params))),
            _ => Err(misfit(Self::KIND)),
        }
    }
}

impl ObservedEvent for SessionStart {
    const KIND: HookEvent = HookEvent::SessionStart;

    fn payload(&self) -> Result<value::Value, CodecError> {
        Ok(value::Value::object([
            ("session", text(self.session)),
            ("workspace", text(self.workspace.as_path().display())),
            ("resumed", value::Value::Bool(self.resumed)),
        ]))
    }
}

impl ObservedEvent for SessionEnd {
    const KIND: HookEvent = HookEvent::SessionEnd;

    fn payload(&self) -> Result<value::Value, CodecError> {
        Ok(value::Value::object([
            ("session", text(self.session)),
            ("reason", text(&self.reason)),
        ]))
    }
}

impl ObservedEvent for ToolResultEvent {
    const KIND: HookEvent = HookEvent::ToolResult;

    fn payload(&self) -> Result<value::Value, CodecError> {
        Ok(value::Value::object([
            ("turn", text(self.turn)),
            ("call", text(self.call.as_str())),
            ("tool", text(self.tool.as_str())),
            ("ok", value::Value::Bool(self.ok)),
            ("preview", text(bounded_preview(&self.preview))),
        ]))
    }
}

impl ObservedEvent for TurnEnd {
    const KIND: HookEvent = HookEvent::TurnEnd;

    fn payload(&self) -> Result<value::Value, CodecError> {
        Ok(value::Value::object([
            ("turn", text(self.turn)),
            ("stop", serialized(&self.stop)?),
        ]))
    }
}

impl ObservedEvent for dal_core::ext::Settled {
    const KIND: HookEvent = HookEvent::Settled;

    fn payload(&self) -> Result<value::Value, CodecError> {
        Ok(value::Value::object([("turn", text(self.turn))]))
    }
}

/// A displayed value as a string field.
fn text(item: impl fmt::Display) -> value::Value {
    value::Value::Str(item.to_string().into())
}

/// A host record as its serialized transport value.
fn serialized(item: &impl serde::Serialize) -> Result<value::Value, CodecError> {
    let json = sonic_rs::to_string(item).map_err(|error| CodecError::Json(error.to_string()))?;
    value::Value::decode(&json)
}

/// The text parts of an input, in order.
fn input_text(content: &[Part]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect()
}

/// Replaces the text parts of an input with `text`, keeping other parts in
/// order after it.
fn replace_text(content: Vec<Part>, text: Box<str>) -> Vec<Part> {
    let rest = content
        .into_iter()
        .filter(|part| !matches!(part, Part::Text { .. }));
    std::iter::once(Part::Text { text }).chain(rest).collect()
}

/// Cuts a preview to [`MAX_PREVIEW`] bytes at a character boundary.
fn bounded_preview(preview: &str) -> &str {
    let mut end = preview.len().min(MAX_PREVIEW);
    while !preview.is_char_boundary(end) {
        end -= 1;
    }
    &preview[..end]
}

/// A hook failure with display text.
fn failed(message: impl Into<Box<str>>) -> HookError {
    HookError::Failed {
        message: message.into(),
    }
}

/// A verdict minted for another event kind; methods refuse to mint one, so
/// this reports a broken invariant rather than script misuse.
fn misfit(kind: HookEvent) -> HookError {
    failed(format!(
        "the returned verdict does not answer a `{}` event",
        kind.as_str()
    ))
}

/// Maps an invocation failure onto the hook error the chain handles.
fn hook_error(failure: InvokeFailure) -> HookError {
    match failure {
        InvokeFailure::Cancelled
        | InvokeFailure::Terminal(dal_agent::ext::script::HostTerminal::Cancelled) => {
            HookError::Cancelled
        }
        other => failed(other.to_string()),
    }
}
