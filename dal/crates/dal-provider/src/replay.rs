//! Replay capture and chaos for provider streams.
//!
//! [`record`] tees an [`EventStream`]'s events into a [`Capture`]; the capture
//! re-encodes each step into the same JSONL grammar [`Script::from_replay`]
//! accepts, so a stream observed live — scripted, replayed, or real — becomes
//! a fixture without hand-authoring. [`chaos`] wraps a stream with declared
//! faults (cuts, injected errors, injected events, latency) so boundary tests
//! exercise the loop's failure paths without patching transport internals.

use std::time::Duration;

use futures::stream;

use dal_core::RawJson;

use crate::{
    error::ProviderError,
    scripted::{Script, ScriptError},
    stream::{EventStream, StopReason, StreamEvent, ToolArgs},
};

/// Streams captured from provider streams, grouped per stream step.
///
/// One events step closes on its `Stop` terminal, or when the stream errors
/// or ends: each `record` wrapper holds its own in-progress buffer and
/// publishes one sealed step atomically, so two live streams wrapped with
/// clones of one capture can never interleave. A stream that ends without
/// `Stop` seals its events step and appends a `fail` line, keeping the
/// fixture a valid replay of the observed failure.
#[derive(Clone, Default)]
pub struct Capture(std::sync::Arc<std::sync::Mutex<Vec<Line>>>);

enum Line {
    /// A reserved slot a live stream has not sealed yet.
    Vacant,
    Events(Vec<StreamEvent>),
    Fail(String),
}

impl Capture {
    /// One capture with no recorded events.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every sealed events step, in operation-start order.
    #[must_use]
    pub fn steps(&self) -> Vec<Vec<StreamEvent>> {
        self.lock()
            .iter()
            .filter_map(|line| match line {
                Line::Events(events) => Some(events.clone()),
                Line::Vacant | Line::Fail(_) => None,
            })
            .collect()
    }

    /// Every recorded stream-failure message, in operation-start order.
    #[must_use]
    pub fn failures(&self) -> Vec<String> {
        self.lock()
            .iter()
            .filter_map(|line| match line {
                Line::Fail(message) => Some(message.clone()),
                Line::Vacant | Line::Events(_) => None,
            })
            .collect()
    }

    /// The recorded steps re-encoded as replay fixture lines.
    ///
    /// Stream failures encode as `fail` steps carrying the error's display
    /// text; replaying the fixture yields [`ProviderError::InvalidRequest`]
    /// in their place.
    ///
    /// # Errors
    /// Returns `Err` when an event cannot encode into the replay grammar or
    /// the joined fixture fails the grammar.
    pub fn replay(&self) -> Result<Vec<String>, ReplayError> {
        let lines = self
            .lock()
            .iter()
            .filter_map(|line| match line {
                // A live stream's reserved slots are not sealed yet: the
                // fixture keeps only completed operations.
                Line::Vacant => None,
                // A partial events step encodes without per-line grammar
                // checks: its terminating `fail` step is a separate line, so
                // the pair validates together against `from_replay` below.
                Line::Events(events) => Some(encode_events(events)),
                Line::Fail(message) => Some(Ok(format!(
                    "{{\"kind\":\"fail\",\"message\":{}}}",
                    json_str(&scrub(message))
                ))),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Script::from_replay(lines.join("\n").as_bytes()).map_err(ReplayError::Grammar)?;
        Ok(lines)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Line>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reserves one operation's two slots — events step plus failure line —
    /// at wrap time, so concurrent streams ordered A then B serialize their
    /// steps as A then B even when B reaches its terminal first.
    fn reserve(&self) -> usize {
        let mut lines = self.lock();
        lines.push(Line::Vacant);
        lines.push(Line::Vacant);
        lines.len() - 2
    }

    /// Fills the reserved slots under one lock: the events step and the
    /// failure line land together and in reservation order.
    fn fill(&self, slot: usize, sealed: Option<Vec<StreamEvent>>, failure: Option<String>) {
        let mut lines = self.lock();
        if let Some(events) = sealed {
            lines[slot] = Line::Events(events);
        }
        if let Some(message) = failure {
            lines[slot + 1] = Line::Fail(message);
        }
    }
}

/// Wraps `stream` so every delivered event is appended to `capture`.
///
/// The wrapper owns its in-progress buffer: a `Stop` seals one events step
/// into the shared capture atomically. An error seals the open step and
/// appends a `fail` line; the stream ending mid-step seals the tail and
/// appends a `fail` line, so a cut stays replayable.
pub fn record(stream: EventStream, capture: Capture) -> EventStream {
    // The step buffer and failure dedupe live behind one mutex shared with
    // the cancel path: a dropped or cancelled stream seals its open step and
    // appends the `fail` line instead of losing the prefix.
    // The step slots are reserved when the operation wraps: completion
    // order never reorders the fixture relative to call order.
    let slot = capture.reserve();
    let shared = std::sync::Arc::new(std::sync::Mutex::new(RecordCx {
        open: Vec::new(),
        failed: false,
    }));
    let source = stream::unfold(
        (stream, capture.clone(), shared.clone()),
        move |(mut inner, capture, shared)| async move {
            let item = inner.next().await;
            let (sealed, failure) = {
                let mut cx = shared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match &item {
                    Some(Ok(event)) => {
                        cx.open.push(event.clone());
                        // A new events block follows any recorded failure, so
                        // the dedupe flag re-arms for this step.
                        cx.failed = false;
                        let sealed = matches!(event, StreamEvent::Stop { .. })
                            .then(|| std::mem::take(&mut cx.open));
                        (sealed, None)
                    }
                    Some(Err(error)) => {
                        let sealed = (!cx.open.is_empty()).then(|| std::mem::take(&mut cx.open));
                        let failure = (!cx.failed).then(|| {
                            cx.failed = true;
                            error.to_string()
                        });
                        (sealed, failure)
                    }
                    None => {
                        // A Stop already sealed the step: only an unterminated
                        // tail earns a `fail` line, so a clean stream records
                        // exactly its `events` step.
                        let (sealed, failure) = if cx.open.is_empty() {
                            (None, None)
                        } else {
                            (
                                Some(std::mem::take(&mut cx.open)),
                                (!cx.failed).then(|| {
                                    cx.failed = true;
                                    "stream ended without a terminal event".to_owned()
                                }),
                            )
                        };
                        (sealed, failure)
                    }
                }
            };
            capture.fill(slot, sealed, failure);
            item.map(|item| (item, (inner, capture, shared)))
        },
    );
    EventStream::new(source, move || {
        let (sealed, failure) = {
            let mut cx = shared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cx.open.is_empty() {
                (None, None)
            } else {
                (
                    Some(std::mem::take(&mut cx.open)),
                    (!cx.failed).then(|| {
                        cx.failed = true;
                        "stream dropped mid-step".to_owned()
                    }),
                )
            }
        };
        capture.fill(slot, sealed, failure);
    })
}

/// One in-progress events step plus the per-stream failure dedupe flag,
/// shared between the unfold loop and the cancel path.
struct RecordCx {
    open: Vec<StreamEvent>,
    failed: bool,
}

/// Faults injected into one provider stream, addressed by event index.
///
/// Indexing counts items the inner stream would yield (events and errors
/// alike) before any chaos adjustment.
#[derive(Debug, Default)]
pub struct Chaos {
    /// The source ends after this many items without a terminal; downstream
    /// sees [`ProviderError::StreamCut`] and the inner stream is dropped, so
    /// its transport cancellation still runs.
    pub cut_after: Option<usize>,
    /// At this index the stream yields this error instead of the inner item.
    pub fail_at: Option<(usize, ProviderError)>,
    /// Items yielded at these indices, replacing the inner ones.
    pub inject_at: Vec<(usize, StreamEvent)>,
    /// Latency added before every yielded item.
    pub delay: Option<Duration>,
}

impl Chaos {
    /// A stream cut after `after` yielded items.
    #[must_use]
    pub fn cut(after: usize) -> Self {
        Self {
            cut_after: Some(after),
            ..Self::default()
        }
    }

    /// A stream that yields `error` at index `at`.
    #[must_use]
    pub fn fail(at: usize, error: ProviderError) -> Self {
        Self {
            fail_at: Some((at, error)),
            ..Self::default()
        }
    }

    /// A stream yielding `event` at index `at` in place of the inner item.
    #[must_use]
    pub fn inject(at: usize, event: StreamEvent) -> Self {
        Self {
            inject_at: vec![(at, event)],
            ..Self::default()
        }
    }
}

/// Wraps `stream` so the yielded sequence follows `spec`.
///
/// `fail_at` wins over `inject_at` at the same index; `cut_after` counts
/// yielded items, injected ones included.
pub fn chaos(stream: EventStream, spec: Chaos) -> EventStream {
    let source = stream::unfold(
        (Some(stream), spec, 0usize),
        |(inner, mut spec, index)| async move {
            if spec.cut_after.is_some_and(|cut| index >= cut) {
                return None;
            }
            let mut inner = inner?;
            let next = inner.next().await;
            let item = if matches!(&spec.fail_at, Some((at, _)) if *at == index) {
                spec.fail_at.take().map(|(_, error)| Err(error))
            } else if let Some((_, event)) = spec.inject_at.iter().find(|(at, _)| *at == index) {
                Some(Ok(event.clone()))
            } else {
                next
            };
            if item.is_some()
                && let Some(delay) = spec.delay
            {
                tokio::time::sleep(delay).await;
            }
            item.map(|item| {
                // A terminal item ends the stream: drop the inner stream
                // now (running its cancellation) so no later source item
                // can reach `next` past the terminal guard.
                let inner = (!terminal(&item)).then_some(inner);
                (item, (inner, spec, index + 1))
            })
        },
    );
    EventStream::new(source, || {})
}

/// Whether a yielded item ends the stream under the `EventStream`
/// contract: a `Stop` terminal or an error.
fn terminal(item: &Result<StreamEvent, ProviderError>) -> bool {
    matches!(item, Ok(StreamEvent::Stop { .. }) | Err(_))
}

/// The replay grammar could not represent a stream step.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    /// A stream event has no replay encoding.
    #[error("event {index} cannot encode into the replay grammar: {detail}")]
    Encode {
        /// The index of the offending event inside its step.
        index: usize,
        /// What could not be encoded, in one short phrase.
        detail: &'static str,
    },
    /// The encoded step fails the replay grammar.
    #[error("encoded step fails the replay grammar: {0}")]
    Grammar(#[source] ScriptError),
}

/// Encodes one event into its replay wire shape.
///
/// `ToolArgsDelta` fragments may split a UTF-8 sequence, so they re-encode
/// as the `fragment` string only when the bytes are valid UTF-8; otherwise
/// they carry the lossless `fragment_bytes` array.
///
/// # Errors
/// Returns `Err` when the event cannot encode into the replay grammar or the
/// encoded wire is not valid JSON.
pub fn encode_event(event: &StreamEvent) -> Result<RawJson, ReplayError> {
    let text = match event {
        StreamEvent::TextDelta { text } => {
            format!("{{\"type\":\"text_delta\",\"text\":{}}}", json_str(text))
        }
        StreamEvent::ReasoningDelta { text } => {
            format!(
                "{{\"type\":\"reasoning_delta\",\"text\":{}}}",
                json_str(text)
            )
        }
        StreamEvent::ToolCallStarted { id, name } => format!(
            "{{\"type\":\"tool_call_started\",\"id\":{},\"name\":{}}}",
            json_str(id),
            json_str(name)
        ),
        StreamEvent::ToolArgsDelta { id, fragment } => {
            if let Ok(text) = str::from_utf8(fragment) {
                format!(
                    "{{\"type\":\"tool_args_delta\",\"id\":{},\"fragment\":{}}}",
                    json_str(id),
                    json_str(text)
                )
            } else {
                let bytes = fragment
                    .iter()
                    .map(u8::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "{{\"type\":\"tool_args_delta\",\"id\":{},\"fragment_bytes\":[{bytes}]}}",
                    json_str(id)
                )
            }
        }
        StreamEvent::Replay { payload } => {
            // `item` must reach the same family/model verbatim: scrub must
            // never rewrite inside it, so a credential-shaped payload
            // refuses export instead of silently mutating replay bytes.
            if scrub(payload.item.as_str()) != payload.item.as_str() {
                return Err(encode_err(
                    "replay payload carries credential-shaped bytes; redact the capture first",
                ));
            }
            // Raw-text scrubbing sees escapes, not decoded text: `"ak\u0049A..."`
            // decodes into a credential the byte scan cannot see, so decoded
            // leaves (keys included) go through the same check.
            if let Ok(item) = sonic_rs::from_str::<sonic_rs::Value>(payload.item.as_str())
                && decoded_secret(&item)
            {
                return Err(encode_err(
                    "replay payload decodes to credential-shaped text; redact the capture first",
                ));
            }
            let family = sonic_rs::to_string(&payload.family)
                .map_err(|_| encode_err("family not serializable"))?;
            let model = sonic_rs::to_string(&payload.model)
                .map_err(|_| encode_err("model not serializable"))?;
            format!(
                "{{\"type\":\"replay\",\"payload\":{{\"family\":{family},\"model\":{model},\"item\":{}}}}}",
                payload.item.as_str()
            )
        }
        StreamEvent::ToolCallsDone { calls } => {
            let calls = calls
                .iter()
                .map(|call| {
                    let args = match &call.args {
                        ToolArgs::Parsed(value) => {
                            format!("{{\"kind\":\"parsed\",\"value\":{}}}", value.as_str())
                        }
                        ToolArgs::Invalid { message } => {
                            format!("{{\"kind\":\"invalid\",\"message\":{}}}", json_str(message))
                        }
                        ToolArgs::Truncated => "{\"kind\":\"truncated\"}".to_owned(),
                    };
                    format!(
                        "{{\"id\":{},\"name\":{},\"args\":{args}}}",
                        json_str(&call.id),
                        json_str(&call.name)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{\"type\":\"tool_calls_done\",\"calls\":[{calls}]}}")
        }
        StreamEvent::Usage { usage } => {
            let usage =
                sonic_rs::to_string(usage).map_err(|_| encode_err("usage not serializable"))?;
            format!("{{\"type\":\"usage\",\"usage\":{usage}}}")
        }
        StreamEvent::Stop { reason } => {
            let reason = match reason {
                StopReason::EndTurn => "end_turn".to_owned(),
                StopReason::ToolUse => "tool_use".to_owned(),
                StopReason::MaxTokens => "max_tokens".to_owned(),
                StopReason::Refusal => "refusal".to_owned(),
                StopReason::Paused => "paused".to_owned(),
                StopReason::Other(reason) => reason.clone(),
            };
            format!("{{\"type\":\"stop\",\"reason\":{}}}", json_str(&reason))
        }
    };
    // The exported fixture is meant to be committed, so encode scrubs
    // credential-shaped tokens instead of trusting every producer to
    // redact before `record` saw the event.
    RawJson::parse(&scrub(&text)).map_err(|_| encode_err("encoded wire is not valid JSON"))
}

/// Redacts credential-shaped tokens from exported fixture text.
///
/// Transports already scrub the secrets they own; this lexical pass covers
/// the rest — a provider echoing an authorization header or embedding a
/// token in an error body — by matching the prefixes real credentials
/// take (`Bearer`, `sk-*`, `xox*`, `gh*_`, `AKIA`, JWT `eyJ`) followed by
/// at least eight token characters. A bare authorization scheme needs a
/// stronger signal — sixteen-plus mixed-case characters — so prose words
/// like `basic` never redact the text after them.
fn scrub(text: &str) -> String {
    String::from_utf8(scrub_bytes(text.as_bytes()))
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

/// True when any decoded string leaf of `item` — keys included — carries
/// credential-shaped text. Escaped input like `"sk\u002d..."` hides from a
/// raw-bytes scrub but not from the decoded tree.
fn decoded_secret(item: &sonic_rs::Value) -> bool {
    use sonic_rs::{JsonContainerTrait, JsonValueTrait};
    if let Some(text) = item.as_str() {
        return scrub(text) != text;
    }
    if let Some(items) = item.as_array() {
        return items.iter().any(decoded_secret);
    }
    if let Some(map) = item.as_object() {
        return map
            .iter()
            .any(|(key, value)| scrub(key) != key || decoded_secret(value));
    }
    false
}

fn scrub_bytes(bytes: &[u8]) -> Vec<u8> {
    fn token(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
    }
    /// Authorization schemes; a credential follows their keyword. The
    /// match is case-insensitive — `bearer` and `Basic` hide as easily as
    /// `Bearer`.
    const SCHEMES: &[&str] = &[
        "bearer",
        "basic",
        "digest",
        "negotiate",
        "oauth",
        "token",
        "apikey",
        "api-key",
        "aws4-hmac-sha256",
    ];
    const PREFIXES: &[&str] = &[
        "sk-",
        "sk_live_",
        "sk_test_",
        "sk-ant-",
        "rk-",
        "pk_live_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxr-",
        "xoxs-",
        "xapp-",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "xai-",
        "AIza",
        "AKIA",
        "ASIA",
        "ya29.",
        "dop_v1_",
        "shpat_",
    ];
    let mut out = Vec::with_capacity(bytes.len());
    let mut after_scheme = false;
    let mut at = 0;
    while at < bytes.len() {
        if token(bytes[at]) {
            let start = at;
            while at < bytes.len() && token(bytes[at]) {
                at += 1;
            }
            let word = &bytes[start..at];
            // A bare scheme word is prose too (`use basic functionality`),
            // so the token it precedes must look high-entropy: at least
            // sixteen characters with a non-lowercase byte. Prose words stay
            // lowercase and rarely stretch that far; credentials do.
            let secretish = (after_scheme
                && word.len() >= 16
                && word.iter().any(|byte| !byte.is_ascii_lowercase()))
                || PREFIXES.iter().any(|prefix| {
                    word.starts_with(prefix.as_bytes()) && word.len() >= prefix.len() + 8
                })
                || (word.starts_with(b"eyJ") && word.len() >= 12);
            after_scheme = SCHEMES
                .iter()
                .any(|scheme| word.eq_ignore_ascii_case(scheme.as_bytes()));
            out.extend_from_slice(if secretish { b"[redacted]" } else { word });
        } else {
            // Non-token byte: punctuation, whitespace, or part of a UTF-8
            // multibyte character — copied verbatim either way.
            out.push(bytes[at]);
            at += 1;
        }
    }
    out
}

/// Encodes one step's events into a `{"kind":"events","events":[...]}` line.
///
/// Validates the result against [`Script::from_replay`], so the output is a
/// fixture the scripted provider accepts byte-for-grammar.
///
/// # Errors
/// Returns `Err` when an event cannot encode or the joined line fails the
/// replay grammar.
pub fn encode_step(events: &[StreamEvent]) -> Result<String, ReplayError> {
    let line = encode_events(events)?;
    Script::from_replay(line.as_bytes()).map_err(ReplayError::Grammar)?;
    Ok(line)
}

/// Encodes one step's events into a `{"kind":"events","events":[...]}` line
/// without grammar validation: a partial prefix's `fail` step is a separate
/// line, so a sequence containing it must validate as a whole.
fn encode_events(events: &[StreamEvent]) -> Result<String, ReplayError> {
    let rewritten = rescrub_split_text(events);
    let events = rewritten.as_deref().unwrap_or(events);
    let events = events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            encode_event(event).map_err(|error| match error {
                ReplayError::Encode { detail, .. } => ReplayError::Encode { index, detail },
                error @ ReplayError::Grammar(_) => error,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let joined = events
        .iter()
        .map(RawJson::as_str)
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!("{{\"kind\":\"events\",\"events\":[{joined}]}}"))
}

/// A credential split across event texts survives per-event scrubbing:
/// scrub each channel's concatenated text, then refill the events in
/// order so every emitted byte is redacted. The concatenation is the
/// semantic payload — boundary placement is arbitrary once a redaction
/// lands — so rewritten boundaries approximate the originals. Returns
/// `None` when nothing needed redaction.
fn rescrub_split_text(events: &[StreamEvent]) -> Option<Vec<StreamEvent>> {
    let mut out = events.to_vec();
    let mut changed = false;
    changed |= refill(
        out.iter_mut()
            .filter_map(|event| match event {
                StreamEvent::TextDelta { text } => Some(text),
                _ => None,
            })
            .collect(),
    );
    changed |= refill(
        out.iter_mut()
            .filter_map(|event| match event {
                StreamEvent::ReasoningDelta { text } => Some(text),
                _ => None,
            })
            .collect(),
    );
    // Parallel calls interleave `ToolArgsDelta` events: one call's text can
    // sit inside another's credential, so a joined stream hides what the
    // per-call payload carries. Scrub each call's fragments on their own.
    let mut calls: Vec<String> = Vec::new();
    for event in &out {
        if let StreamEvent::ToolArgsDelta { id, .. } = event
            && !calls.iter().any(|seen| seen == id)
        {
            calls.push(id.clone());
        }
    }
    for call in &calls {
        changed |= refill_bytes(
            out.iter_mut()
                .filter_map(|event| match event {
                    StreamEvent::ToolArgsDelta { id, fragment } if id == call => Some(fragment),
                    _ => None,
                })
                .collect(),
        );
    }
    changed.then_some(out)
}

/// Scrubs the concatenation of `fields`, then redistributes the result:
/// each field keeps its original length until the last one absorbs the
/// remainder. No-op when the joined text carries nothing shaped like a
/// credential.
fn refill(fields: Vec<&mut String>) -> bool {
    let joined: String = fields.iter().map(|field| field.as_str()).collect();
    let scrubbed = scrub(&joined);
    if scrubbed == joined {
        return false;
    }
    let mut cursor = scrubbed.as_str();
    let last = fields.len().saturating_sub(1);
    for (index, field) in fields.into_iter().enumerate() {
        let mut take = if index == last {
            cursor.len()
        } else {
            field.len().min(cursor.len())
        };
        while !cursor.is_char_boundary(take) {
            take -= 1;
        }
        let (head, tail) = cursor.split_at(take);
        head.clone_into(field);
        cursor = tail;
    }
    debug_assert!(cursor.is_empty(), "refill dropped scrubbed bytes");
    true
}

/// Byte-level [`refill`] for tool-arg fragments, which may split a UTF-8
/// character mid-token legitimately.
fn refill_bytes(fields: Vec<&mut Vec<u8>>) -> bool {
    let joined: Vec<u8> = fields
        .iter()
        .flat_map(|field| field.iter().copied())
        .collect();
    let scrubbed = scrub_bytes(&joined);
    if scrubbed == joined {
        return false;
    }
    let mut cursor = scrubbed.as_slice();
    let last = fields.len().saturating_sub(1);
    for (index, field) in fields.into_iter().enumerate() {
        let take = if index == last {
            cursor.len()
        } else {
            field.len().min(cursor.len())
        };
        let (head, tail) = cursor.split_at(take);
        *field = head.to_vec();
        cursor = tail;
    }
    debug_assert!(cursor.is_empty(), "refill dropped scrubbed bytes");
    true
}

fn encode_err(detail: &'static str) -> ReplayError {
    ReplayError::Encode { index: 0, detail }
}

fn json_str(text: &str) -> String {
    sonic_rs::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

#[cfg(test)]
mod tests {
    use dal_core::{Family, RawJson};

    use super::*;
    use crate::scripted::Script;
    use crate::stream::ReplayPayload;

    const FIXTURE: &str = concat!(
        r#"{"kind":"events","events":["#,
        r#"{"type":"text_delta","text":"hello"},"#,
        r#"{"type":"tool_calls_done","calls":[{"id":"c1","name":"read","args":{"kind":"parsed","value":{"path":"f.txt"}}}]},"#,
        r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},"#,
        r#"{"type":"stop","reason":"tool_use"}]}"#,
        "\n",
        r#"{"kind":"events","events":["#,
        r#"{"type":"text_delta","text":"done"},"#,
        r#"{"type":"tool_calls_done","calls":[]},"#,
        r#"{"type":"usage","usage":{"input_tokens":4,"cached_input_tokens":0,"output_tokens":2,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},"#,
        r#"{"type":"stop","reason":"end_turn"}]}"#,
    );

    async fn drain(stream: &mut EventStream) -> Vec<Result<StreamEvent, ProviderError>> {
        let mut items = Vec::new();
        while let Some(item) = stream.next().await {
            items.push(item);
        }
        items
    }

    #[tokio::test]
    async fn record_round_trips_a_stream_into_replay_lines() {
        let capture = Capture::new();
        for _ in 0..2 {
            let script = Script::from_replay(FIXTURE.as_bytes()).expect("fixture parses");
            let mut stream = record(script.open().expect("script opens"), capture.clone());
            let items = drain(&mut stream).await;
            assert!(items.iter().all(Result::is_ok));
        }

        let lines = capture.replay().expect("captured steps encode");
        let first = FIXTURE.lines().next().expect("fixture has lines");
        assert_eq!(lines, vec![first.to_owned(), first.to_owned()]);

        // The emitted fixture round-trips back through the replay decoder.
        Script::from_replay(lines.join("\n").as_bytes()).expect("emitted replay parses");
    }

    /// Exported replays must not carry secrets: scrub recognizes auth
    /// schemes case-insensitively. Reverting to a lowercase-only match
    /// leaks `BEARER <token>` verbatim.
    #[test]
    fn scrub_redacts_authorization_schemes_case_insensitively() {
        for text in [
            "Authorization: BEARER SomeLongTokenValue1234",
            "authorization: DiGeSt abcdef123456789012",
            "x: NEGOTIATE YmFzZTY0LWtleS12YWx1ZQ==",
        ] {
            let redacted = scrub(text);
            assert!(
                redacted.contains("[redacted]"),
                "scheme not redacted in {redacted}"
            );
        }
        // The token alone is still a prefix-shaped secret.
        assert!(scrub("note: no scheme").contains("note: no scheme"));
    }

    /// Bare scheme words are prose too: `use basic functionality` must
    /// export byte-stable, never `use basic [redacted]`. The post-scheme
    /// token only redacts when it looks high-entropy.
    #[test]
    fn scrub_leaves_prose_after_a_scheme_word() {
        for text in [
            "use basic functionality",
            "a token bucket limits retries",
            "digest mode is documented",
        ] {
            assert_eq!(scrub(text), text, "prose was redacted: {text}");
        }
        // A real credential after a scheme still redacts.
        let secret = scrub("Authorization: Bearer Ab3Cd5Ef7Gh9Jk1Lm3N");
        assert!(secret.contains("[redacted]"), "{secret}");
    }

    /// A `replay` payload must reach its family/model verbatim, so scrub
    /// must never rewrite inside it: a credential-shaped item refuses
    /// export instead of silently mutating replay bytes.
    #[test]
    fn encode_rejects_a_credential_shaped_replay_payload() {
        let events = [StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::Anthropic,
                model: "test-model".into(),
                item: RawJson::parse(r#"{"text":"Bearer abc1234567890def"}"#)
                    .expect("payload json"),
            },
        }];
        assert!(
            encode_events(&events).is_err(),
            "a credential-shaped payload encoded"
        );
        // The same shape without secrets exports.
        let clean = [StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::Anthropic,
                model: "test-model".into(),
                item: RawJson::parse(r#"{"text":"plain reasoning"}"#).expect("payload json"),
            },
        }];
        assert!(encode_events(&clean).is_ok());
    }

    /// A credential split across two deltas reassembles inside the joined
    /// export: the capture must rescrub channel text across event
    /// boundaries, not per event. Reverting to per-event scrubbing lets
    /// the split token through.
    #[test]
    fn encode_rescrubs_a_credential_split_across_deltas() {
        let events = [
            StreamEvent::TextDelta {
                text: "key: eyJhbGciOiJIUzI1NiJ".into(),
            },
            StreamEvent::TextDelta {
                text: "9.signature".into(),
            },
        ];
        let lines = encode_events(&events).expect("split deltas encode");
        assert!(
            !lines.contains("eyJhbGciOiJIUzI1NiJ9"),
            "split credential survived: {lines}"
        );
        assert!(lines.contains("[redacted]"), "nothing redacted: {lines}");
    }

    /// Interleaved calls each keep their own byte stream: a second call's
    /// text between one call's `Bearer ` and its token must not consume the
    /// scheme marker. Joining every call's fragments lets it through.
    #[test]
    fn encode_rescrubs_args_fragments_per_call() {
        let events = [
            StreamEvent::ToolArgsDelta {
                id: "a".into(),
                fragment: b"Bearer ".to_vec(),
            },
            StreamEvent::ToolArgsDelta {
                id: "b".into(),
                fragment: br#"{"x":1}"#.to_vec(),
            },
            StreamEvent::ToolArgsDelta {
                id: "a".into(),
                fragment: b"Ab3Cd5Ef7Gh9Jk1L".to_vec(),
            },
        ];
        let lines = encode_events(&events).expect("args deltas encode");
        assert!(
            lines.contains("[redacted]"),
            "cross-call join hid the credential: {lines}"
        );
        // The innocent call's bytes survive untouched — JSON-escaped in
        // the encoded line exactly as delivered.
        assert!(lines.contains(r#"{\"x\":1}"#), "{lines}");
    }

    /// `\uXXXX` escapes decode into text the raw-bytes scrub never sees:
    /// an escaped credential inside a replay payload must still refuse
    /// export. Reverting to raw-text-only scrubbing accepts it.
    #[test]
    fn encode_rejects_an_escaped_credential_in_a_replay_payload() {
        let events = [StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::Anthropic,
                model: "test-model".into(),
                item: RawJson::parse(r#"{"token":"sk-abc123456789012"}"#).expect("payload json"),
            },
        }];
        // Direct form refused by the raw scrub already; the escaped form is
        // the gap the decoded-leaf check closes.
        let escaped = [StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::Anthropic,
                model: "test-model".into(),
                item: RawJson::parse(r#"{"token":"sk-abc123456789012"}"#).expect("payload json"),
            },
        }];
        assert!(encode_events(&events).is_err());
        assert!(encode_events(&escaped).is_err());
        let unicode_escaped = [StreamEvent::Replay {
            payload: ReplayPayload {
                family: Family::Anthropic,
                model: "test-model".into(),
                // `sk` is raw text but the `-` arrives as an escape: the raw
                // scrub sees no `sk-` prefix, the decoded value carries one.
                item: RawJson::parse(r#"{"token":"sk\u002dabc123456789012"}"#)
                    .expect("payload json"),
            },
        }];
        assert!(encode_events(&unicode_escaped).is_err());
    }

    /// Concurrent captures serialize in wrap order, not completion order:
    /// slots are reserved when `record` wraps the stream. Reverting to a
    /// terminal push lets the later-started stream's lines land first.
    #[tokio::test]
    async fn record_preserves_start_order_when_streams_complete_out_of_order() {
        let replay_a = concat!(
            r#"{"kind":"events","events":["#,
            r#"{"type":"text_delta","text":"first-a"},"#,
            r#"{"type":"tool_calls_done","calls":[]},"#,
            r#"{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},"#,
            r#"{"type":"stop","reason":"end_turn"}]}"#,
        );
        let replay_b = concat!(
            r#"{"kind":"events","events":["#,
            r#"{"type":"text_delta","text":"second-b"},"#,
            r#"{"type":"tool_calls_done","calls":[]},"#,
            r#"{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},"#,
            r#"{"type":"stop","reason":"end_turn"}]}"#,
        );
        let capture = Capture::new();
        let mut stream_a = record(
            Script::from_replay(replay_a.as_bytes())
                .expect("script a")
                .open()
                .expect("open a"),
            capture.clone(),
        );
        let mut stream_b = record(
            Script::from_replay(replay_b.as_bytes())
                .expect("script b")
                .open()
                .expect("open b"),
            capture.clone(),
        );
        // B completes first; A's reserved slot still precedes it.
        let _ = drain(&mut stream_b).await;
        let _ = drain(&mut stream_a).await;
        let lines = capture.replay().expect("capture encodes");
        let a_at = lines
            .iter()
            .position(|line| line.contains("first-a"))
            .expect("a's events line");
        let b_at = lines
            .iter()
            .position(|line| line.contains("second-b"))
            .expect("b's events line");
        assert!(a_at < b_at, "completion order leaked into the capture");
    }

    #[tokio::test]
    async fn chaos_cut_delivers_streamcut_and_cancels_the_inner_stream() {
        let script = Script::from_replay(FIXTURE.as_bytes()).expect("fixture parses");
        let mut stream = chaos(script.open().expect("script opens"), Chaos::cut(2));
        let items = drain(&mut stream).await;
        assert_eq!(items.len(), 3, "two real items, then the cut");
        assert!(items[0].is_ok() && items[1].is_ok());
        assert!(
            matches!(&items[2], Err(ProviderError::StreamCut)),
            "expected StreamCut, got {:?}",
            items[2]
        );
    }

    #[tokio::test]
    async fn chaos_fail_yields_the_injected_error_once() {
        let script = Script::from_replay(FIXTURE.as_bytes()).expect("fixture parses");
        let error = ProviderError::Transport {
            family: Family::Responses,
            reason: "chaos injected".to_owned(),
        };
        let mut stream = chaos(script.open().expect("script opens"), Chaos::fail(1, error));
        let items = drain(&mut stream).await;
        assert!(items[0].is_ok());
        match &items[1] {
            Err(ProviderError::Transport { reason, .. }) => {
                assert_eq!(reason, "chaos injected");
            }
            other => panic!("expected the injected error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chaos_inject_replaces_the_inner_item() {
        let script = Script::from_replay(FIXTURE.as_bytes()).expect("fixture parses");
        let injected = StreamEvent::TextDelta {
            text: "injected".to_owned(),
        };
        let mut stream = chaos(
            script.open().expect("script opens"),
            Chaos::inject(0, injected),
        );
        let items = drain(&mut stream).await;
        match &items[0] {
            Ok(StreamEvent::TextDelta { text }) => assert_eq!(text, "injected"),
            other => panic!("expected the injected event, got {other:?}"),
        }
    }

    #[test]
    fn encode_step_rejects_an_ungrammatical_step() {
        let err = encode_step(&[StreamEvent::Stop {
            reason: StopReason::EndTurn,
        }])
        .expect_err("a stop without usage fails the grammar");
        assert!(matches!(err, ReplayError::Grammar(_)), "{err:?}");
    }

    /// A stream dropped mid-step must still seal its partial events step and
    /// append a `fail` line: the fixture records what was observed. Without
    /// the shared cancel state, the prefix silently disappears.
    #[tokio::test]
    async fn record_seals_the_open_step_when_the_stream_drops() {
        let source = stream::iter(vec![Ok(StreamEvent::TextDelta {
            text: "partial".to_owned(),
        })]);
        let inner = EventStream::new(source, || {});
        let capture = Capture::new();
        let mut wrapped = record(inner, capture.clone());
        // Pull the event into the in-progress buffer first: the buffer is
        // what the cancel path seals.
        let _ = wrapped.next().await;
        drop(wrapped);
        let steps = capture.steps();
        assert_eq!(
            steps,
            vec![vec![StreamEvent::TextDelta {
                text: "partial".to_owned()
            }]]
        );
        assert_eq!(capture.failures(), ["stream dropped mid-step"]);
    }

    /// A partial events step plus its `fail` step validate together: the
    /// replay grammar counts the pair as one step, so encoding them per line
    /// rejects a prefix the joined fixture accepts.
    #[tokio::test]
    async fn replay_validates_a_partial_step_with_its_fail_line() {
        let source = stream::iter(vec![
            Ok(StreamEvent::TextDelta {
                text: "partial".to_owned(),
            }),
            Err(ProviderError::StreamCut),
        ]);
        let inner = EventStream::new(source, || {});
        let capture = Capture::new();
        let mut wrapped = record(inner, capture.clone());
        let _ = drain(&mut wrapped).await;
        let lines = capture.replay().expect("the pair validates");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].contains("\"fail\""), "{}", lines[1]);
        Script::from_replay(lines.join("\n").as_bytes()).expect("joined fixture parses");
    }
}

#[cfg(test)]
mod dev_review_tests {
    //! Boundary proofs for the dev-helper review fixes: atomic capture
    //! pairs, chaos terminal drop, and credential scrubbing on export.

    use super::*;

    /// A chaos-injected terminal must end the stream: the inner source
    /// is dropped so a trailing source item cannot reach `next` past the
    /// terminal guard. Reverting the drop yields the trailing item, and
    /// the guard panics in debug builds.
    #[tokio::test]
    async fn chaos_terminal_drop_ends_the_inner_stream() {
        let source = stream::iter(vec![
            Ok(StreamEvent::TextDelta { text: "one".into() }),
            Ok(StreamEvent::TextDelta { text: "two".into() }),
        ]);
        let mut stream = chaos(
            EventStream::new(source, || {}),
            Chaos::inject(
                0,
                StreamEvent::Stop {
                    reason: StopReason::EndTurn,
                },
            ),
        );
        assert!(matches!(
            stream.next().await,
            Some(Ok(StreamEvent::Stop { .. }))
        ));
        assert!(stream.next().await.is_none());
    }

    /// Each stream's sealed step and its failure line land under one
    /// lock, so a second live stream cannot interleave between them —
    /// `replay` proves the pairs stay adjacent.
    #[tokio::test]
    async fn capture_pairs_each_step_with_its_failure() {
        let capture = Capture::default();
        let source = |tag: &str| {
            stream::iter(vec![
                Ok(StreamEvent::TextDelta {
                    text: tag.to_owned(),
                }),
                Err(ProviderError::InvalidRequest {
                    message: format!("stream {tag} blew up"),
                }),
            ])
        };
        let mut a = record(EventStream::new(source("a"), || {}), capture.clone());
        let mut b = record(EventStream::new(source("b"), || {}), capture.clone());
        // Drive both into their buffered step before either ends so the
        // sealed pairs are the only shape replay can parse.
        let _ = a.next().await;
        let _ = b.next().await;
        while a.next().await.is_some() {}
        while b.next().await.is_some() {}
        let fixture = capture.replay().expect("each pair stays adjacent");
        assert_eq!(fixture.len(), 4, "{fixture:?}");
        assert_eq!(capture.steps().len(), 2);
        assert_eq!(capture.failures().len(), 2);
    }

    /// A raw capture carries the wire verbatim, but the exported replay
    /// script must never leak credential-shaped material: the export
    /// scrubs token shapes out of the fixture text.
    #[tokio::test]
    async fn record_scrubs_credential_shapes_from_the_fixture() {
        let capture = Capture::default();
        let source = stream::iter(vec![Err(ProviderError::InvalidRequest {
            message: "upstream sent Bearer sk-live-abcdefghij1234".to_owned(),
        })]);
        let mut stream = record(EventStream::new(source, || {}), capture.clone());
        while stream.next().await.is_some() {}
        let fixture = capture.replay().expect("fixture parses");
        let text = fixture.join("\n");
        assert!(!text.contains("sk-live-abcdefghij1234"), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
    }
}
