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
    Events(Vec<StreamEvent>),
    Fail(String),
}

impl Capture {
    /// One capture with no recorded events.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every sealed events step, in arrival order.
    #[must_use]
    pub fn steps(&self) -> Vec<Vec<StreamEvent>> {
        self.lock()
            .iter()
            .filter_map(|line| match line {
                Line::Events(events) => Some(events.clone()),
                Line::Fail(_) => None,
            })
            .collect()
    }

    /// Every recorded stream-failure message, in arrival order.
    #[must_use]
    pub fn failures(&self) -> Vec<String> {
        self.lock()
            .iter()
            .filter_map(|line| match line {
                Line::Fail(message) => Some(message.clone()),
                Line::Events(_) => None,
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
            .map(|line| match line {
                // A partial events step encodes without per-line grammar
                // checks: its terminating `fail` step is a separate line, so
                // the pair validates together against `from_replay` below.
                Line::Events(events) => encode_events(events),
                Line::Fail(message) => Ok(format!(
                    "{{\"kind\":\"fail\",\"message\":{}}}",
                    json_str(&scrub(message))
                )),
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

    /// Appends a sealed step and its failure line under one lock, so a
    /// second live stream cannot interleave a line between them.
    fn push(&self, sealed: Option<Vec<StreamEvent>>, failure: Option<String>) {
        let mut lines = self.lock();
        if let Some(events) = sealed {
            lines.push(Line::Events(events));
        }
        if let Some(message) = failure {
            lines.push(Line::Fail(message));
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
    let shared = std::sync::Arc::new(std::sync::Mutex::new(RecordCx {
        open: Vec::new(),
        failed: false,
    }));
    let source = stream::unfold(
        (stream, capture.clone(), shared.clone()),
        |(mut inner, capture, shared)| async move {
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
            capture.push(sealed, failure);
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
        capture.push(sealed, failure);
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
/// at least eight token characters.
fn scrub(text: &str) -> String {
    fn token(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
    }
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
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut after_bearer = false;
    let mut at = 0;
    while at < bytes.len() {
        if token(bytes[at]) {
            let start = at;
            while at < bytes.len() && token(bytes[at]) {
                at += 1;
            }
            let word = &text[start..at];
            let secretish = (after_bearer && word.len() >= 8)
                || PREFIXES
                    .iter()
                    .any(|prefix| word.starts_with(prefix) && word.len() >= prefix.len() + 8)
                || (word.starts_with("eyJ") && word.len() >= 12);
            after_bearer = word == "Bearer";
            out.push_str(if secretish { "[redacted]" } else { word });
        } else {
            // Non-token byte: ASCII punctuation or a UTF-8 lead byte — copy
            // the whole character.
            let width = text[at..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&text[at..at + width]);
            at += width;
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

fn encode_err(detail: &'static str) -> ReplayError {
    ReplayError::Encode { index: 0, detail }
}

fn json_str(text: &str) -> String {
    sonic_rs::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

#[cfg(test)]
mod tests {
    use dal_core::Family;

    use super::*;
    use crate::scripted::Script;

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
