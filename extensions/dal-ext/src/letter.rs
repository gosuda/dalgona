//! Deterministic rendering of text with the bundled bitmap font.
//!
//! Glyph data: GNU Unifont 18.0.01 (`unifont-18.0.01.hex`), used under the
//! SIL Open Font License, Version 1.1 (full text in `OFL-1.1.txt`).
//! Upstream: <https://unifoundry.com/unifont>/
//! Copyright (C) 1998-2026 Roman Czyborra, Paul Hardy, Qianqian Fang, Andrew Miller, Johnnie Weaver, David Corbett, Ælla Chiana Moskopp, Rebecca Bettencourt, Minseo Lee, Ho-Seok Ee, et al.
use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use dal_agent::error::SchemeError;
use dal_agent::ext::{
    BoxFuture, Doc, Extension, ExtensionBuilder, Hook, HookCx, HookError, ObserveHook, RawValue,
    SchemeCx, SchemeResolver,
};
use dal_core::ext::SessionEnd;
use dal_core::{
    BlobId, InputEvent, InputVerdict, Notice, Part, RegistrationError, ServiceSet, SessionId,
};
use serde::{Deserialize, Serialize};

mod budget;
mod glyphs;
mod layout;

pub use budget::{
    BYTE_BUDGET, FallbackReason, IMAGE_BUDGET, LetterAssembly, LetterChunk, LetterFallback,
    image_parts, letters,
};
pub use glyphs::{Font, Glyph, GlyphError, Glyphs};
pub use layout::{
    DOUBLE_W, DrawError, DrawOutcome, GLYPH_H, Image, LINE_H, MARGIN, MAX_DESC_CHARS, SINGLE_W,
    TAB_STOP, WRAP_CELLS, draw, is_zero_width,
};

/// Kind discriminator for a rendered skill letter record.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LetterKind {
    /// A bitmap generated from a skill description.
    #[serde(rename = "skill")]
    Skill,
}

/// Strict journal body for one persisted skill image and its exact source.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillLetterRecord {
    /// Record schema version. Only `1` is supported.
    pub v: u8,
    /// Decimal image id in `1..=20`.
    pub id: Box<str>,
    /// Record kind.
    pub kind: LetterKind,
    /// Registered skill name.
    pub skill: Box<str>,
    /// Plugin that registered the skill.
    pub plugin: Box<str>,
    /// Content digest of the PNG blob.
    pub png_blob: BlobId,
    /// Content digest of the exact source-description blob.
    pub source_blob: BlobId,
    /// PNG byte length.
    pub png_bytes: u64,
    /// PNG width in pixels.
    pub width: u32,
    /// PNG height in pixels.
    pub height: u32,
    /// Single-width cell size in pixels.
    pub cell: [u8; 2],
}

/// Capture state for one session's first-prompt image set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LetterState {
    /// No durable image records have been acknowledged.
    Empty,
    /// The first-prompt image set is durable and cannot change.
    Frozen,
}

/// Captured letter ids and their session-level state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LetterSession {
    /// Whether this session has committed its first-prompt image set.
    pub state: LetterState,
    /// Admitted chunks in numeric id order.
    pub ids: Arc<[LetterChunk]>,
}

/// Exact `letter://` resolver error text.
const LETTER_ID_MALFORMED_PREFIX: &str = "letter id is malformed";
const LETTER_NOT_IN_SESSION_INFIX: &str = "does not exist in this session";
const LETTER_SOURCE_GONE_SUFFIX: &str = "source blob is gone";

/// Renders `letter id is malformed: <id>` for a non-canonical numeric path.
#[must_use]
pub fn malformed_id_error(id: &str) -> String {
    format!("{LETTER_ID_MALFORMED_PREFIX}: {id}")
}

/// Renders `letter <n> does not exist in this session`.
#[must_use]
pub fn missing_id_error(id: &str) -> String {
    format!("letter {id} {LETTER_NOT_IN_SESSION_INFIX}")
}

/// Renders `letter <n> source blob is gone`.
#[must_use]
pub fn gone_source_error(id: &str) -> String {
    format!("letter {id} {LETTER_SOURCE_GONE_SUFFIX}")
}

/// A `letter://` path without its scheme prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LetterRoute {
    /// Empty path: the skill/history index.
    Empty,
    /// Well-formed numeric id as decimal text (syntax only; existence is
    /// resolved later, with no size cap).
    Numeric(Box<str>),
    /// Non-numeric id delegated to the history source index.
    History,
    /// Leading zero, sign, whitespace, non-ASCII digits, or other bad syntax.
    Malformed,
}

/// Classifies a `letter://` path without touching stores or records.
///
/// All-ASCII-digit paths with no leading zero are `Numeric` regardless of
/// size; the `1..=20` admission range applies to stored skill records only,
/// so a well-formed but unstored number (for example `21`) routes to
/// `Numeric` and later reports the missing-id literal. Paths containing `/`
/// (for example `history/1.1`) are `History` and delegate to the history
/// source index. Anything else (including bare names such as `x`, which the
/// `letter-uri-reads` contract requires to report the malformed-id literal)
/// is `Malformed`.
#[must_use]
pub fn classify_letter_path(path: &str) -> LetterRoute {
    if path.is_empty() {
        return LetterRoute::Empty;
    }
    let bytes = path.as_bytes();
    if !bytes.is_empty() && bytes.iter().all(u8::is_ascii_digit) {
        if bytes.first() != Some(&b'0') {
            return LetterRoute::Numeric(path.into());
        }
        return LetterRoute::Malformed;
    }
    if path.contains('/') {
        return LetterRoute::History;
    }
    LetterRoute::Malformed
}

/// Validates a numeric `letter://` path syntactically (no admission range).
///
/// Returns the number, or `None` for empty input, a leading zero,
/// non-ASCII digits, a sign, or a value wider than `u64`.
#[must_use]
pub fn parse_letter_id(id: &str) -> Option<u64> {
    match classify_letter_path(id) {
        LetterRoute::Numeric(text) => text.parse().ok(),
        LetterRoute::Empty | LetterRoute::History | LetterRoute::Malformed => None,
    }
}

/// Renders the diagnostic notice for an undrawable skill description.
///
/// The single codepoint renders as four or more uppercase hex digits.
#[must_use]
pub fn undrawable_notice(skill: &str, plugin: &str, codepoint: u32) -> String {
    format!(
        "letter '{skill}' from plugin '{plugin}' fell back to text: first undrawable codepoint U+{codepoint:04X}."
    )
}

/// Renders the diagnostic notice for an over-budget image candidate.
#[must_use]
pub fn over_budget_notice(skill: &str, plugin: &str) -> String {
    format!(
        "letter '{skill}' from plugin '{plugin}' fell back to text: image byte budget exceeded."
    )
}

/// Renders the diagnostic notice for another renderer failure.
#[must_use]
pub fn render_failed_notice(skill: &str, plugin: &str) -> String {
    format!("letter '{skill}' from plugin '{plugin}' fell back to text: render failed.")
}

/// Renders the notice for an input whose model does not read images: no
/// letter is drawn or stored and the input stays ordinary text.
#[must_use]
pub fn no_image_model_notice() -> &'static str {
    "letter: the model does not read images; skill letters were not attached."
}

/// Renders the notice for an input whose model did not resolve, so its image
/// support is unknown: the hook never attaches images blind.
#[must_use]
pub fn unknown_model_notice() -> &'static str {
    "letter: the model's image support is unknown; skill letters were not attached."
}

/// Renders the notice for a failed letter hook, carrying the failure text
/// the host's fixed `hook input failed` notice drops.
#[must_use]
pub fn not_persisted_notice(cause: &str) -> String {
    format!(
        "letter: skill letters were not persisted: {}.",
        cause.trim_end_matches('.')
    )
}

impl SkillLetterRecord {
    /// Validates strict record bounds: `v == 1`, `kind == Skill`, canonical id
    /// in `1..=20`, `cell == [8, 16]`, `width <= 784`, `height <= 1392`.
    #[must_use]
    pub fn validated(&self) -> Option<u8> {
        if self.v != 1
            || !matches!(self.kind, LetterKind::Skill)
            || self.cell != [8, 16]
            || self.width > 784
            || self.height > 1392
        {
            return None;
        }
        let number = parse_letter_id(&self.id)?;
        u8::try_from(number)
            .ok()
            .filter(|number| (1..=20).contains(number))
    }
}

/// Reads admitted skill-letter sources: `letter://<id>`.
///
/// Numeric ids resolve against the session's persisted `letter` records: each
/// body strictly decodes to [`SkillLetterRecord`], validates, and contributes
/// its stored `source_blob` digest, which is read through the host blob store.
/// Records are the only source of truth, so restart, plugin reload, and fork
/// cannot change the image set. An id with no stored record reports the
/// missing literal; a stored digest with no blob reports the gone literal.
/// The empty path lists stored entries in numeric order followed by the host
/// history lines. Slash paths (for example `history/1.1`) delegate to the
/// host history index with its errors preserved byte-exact; bare non-numeric
/// names report the malformed-id literal. The host owns all blob and record
/// persistence; this resolver never publishes.
#[derive(Debug, Clone, Copy, Default)]
pub struct LetterResolver;

impl LetterResolver {
    /// Builds the stateless resolver; session truth comes from stored records.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Strictly decodes one stored body; corrupt bytes fail the read.
///
/// Well-formed bodies that fail [`validated`] are the caller's decision:
/// the numeric arm fails only when the requested id itself is invalid and
/// otherwise skips the row; the empty arm lists admitted rows only. Either
/// way no corrupt row can produce fabricated source text.
fn decode_record(body: &RawValue) -> Result<SkillLetterRecord, SchemeError> {
    body.decode_as::<SkillLetterRecord>()
        .map_err(|error| SchemeError::Failed {
            message: error.to_string().into(),
        })
}

/// Builds the validation-failure error for one stored id.
fn invalid_record_error(id: &str) -> SchemeError {
    SchemeError::Failed {
        message: format!("letter record '{id}' failed validation").into(),
    }
}

/// Fetches this session's stored `letter` bodies for one resolver read.
async fn stored_bodies(cx: &SchemeCx<'_>) -> Result<Vec<Box<RawValue>>, SchemeError> {
    cx.services()
        .records(cx.caller(), "letter")
        .await
        .map_err(|error| SchemeError::Failed {
            message: error.to_string().into(),
        })
}

impl SchemeResolver for LetterResolver {
    fn read<'a>(
        &'a self,
        path: &'a str,
        cx: &'a SchemeCx<'a>,
    ) -> BoxFuture<'a, Result<Doc, SchemeError>> {
        Box::pin(async move {
            match classify_letter_path(path) {
                LetterRoute::Empty => {
                    let bodies = stored_bodies(cx).await?;
                    let mut admitted: Vec<(u8, SkillLetterRecord)> = Vec::new();
                    for body in &bodies {
                        let record = decode_record(body)?;
                        if let Some(number) = record.validated() {
                            admitted.push((number, record));
                        }
                    }
                    admitted.sort_by_key(|(number, _)| *number);
                    let mut lines: Vec<String> = admitted
                        .iter()
                        .map(|(_, record)| {
                            format!(
                                "letter://{}  skill '{}' from plugin '{}'",
                                record.id, record.skill, record.plugin
                            )
                        })
                        .collect();
                    let history = cx.letter_index().index_lines(cx.session).await?;
                    lines.extend(history.iter().map(std::string::ToString::to_string));
                    Ok(Doc::new("letter://", lines.join("\n")))
                }
                LetterRoute::Numeric(id) => {
                    let bodies = stored_bodies(cx).await?;
                    let mut matched: Option<SkillLetterRecord> = None;
                    for body in &bodies {
                        let record = decode_record(body)?;
                        if record.validated().is_none() {
                            if *record.id == *id {
                                return Err(invalid_record_error(&id));
                            }
                            continue;
                        }
                        if *record.id == *id {
                            matched = Some(record);
                            break;
                        }
                    }
                    let Some(record) = matched else {
                        return Err(SchemeError::Failed {
                            message: missing_id_error(&id).into(),
                        });
                    };
                    let stored = cx.blob_get(&record.source_blob).await?;
                    let Some(bytes) = stored else {
                        return Err(SchemeError::Failed {
                            message: gone_source_error(&id).into(),
                        });
                    };
                    let text = String::from_utf8(bytes).map_err(|error| SchemeError::Failed {
                        message: error.to_string().into(),
                    })?;
                    Ok(Doc::new(format!("letter://{path}"), text))
                }
                LetterRoute::History => {
                    let bytes = cx.letter_index().read(cx.session, path).await?;
                    let text =
                        String::from_utf8(bytes.to_vec()).map_err(|error| SchemeError::Failed {
                            message: error.to_string().into(),
                        })?;
                    Ok(Doc::new(format!("letter://{path}"), text))
                }
                LetterRoute::Malformed => Err(SchemeError::Failed {
                    message: malformed_id_error(path).into(),
                }),
            }
        })
    }
}

/// Record kind for image-delivery markers. The session actor journals one
/// marker in the same batch as any admitted user entry that carries image
/// parts; its digests name every admitted image, letter or otherwise. The
/// actor cannot import this crate, so its literal must stay byte-equal to
/// this: the letter tests pin the round trip.
const DELIVERED_KIND: &str = "letter_delivered";

/// One delivery marker: the image digests a single admitted user entry
/// carried. Markers are advisory, so undecodable rows are skipped and a
/// missing marker only re-attaches images; corrupt letter rows still fail
/// the read.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LetterDelivered {
    v: u8,
    digests: Vec<BlobId>,
}

/// First-input hook: persists the first-prompt letter set through the host
/// services seam, then delivers its images with the running content.
///
/// Blobs and records go through `cx.services` (`blob_put`, `append_record`),
/// whose futures resolve only once durable; the hook never touches journal
/// or blob paths directly. Delivery returns [`InputVerdict::Transform`] with
/// the admitted images followed by the running parts, so the folded `User`
/// record and the provider request carry the same final parts. An empty
/// assembly, empty input, a frozen set, or a model that does not read images
/// returns [`InputVerdict::Continue`]: the input stays ordinary text.
/// Freezing rests on delivery markers, not on records alone: a stored set
/// whose images never reached an admitted request is re-attached on the next
/// input, so each letter is delivered in exactly one admitted request.
#[derive(Debug, Clone)]
struct FirstInputHook {
    registry: Arc<crate::skills::SkillRegistry>,
    font: Arc<Font>,
    /// Sessions already told that their model gets no letters, so the notice
    /// appears once per open session rather than on every input.
    told: Arc<Mutex<HashSet<SessionId>>>,
    /// The letter assembly, drawn once: it is a pure function of the
    /// immutable registry and font, so later inputs reuse it instead of
    /// re-encoding up to twenty PNGs.
    memo: Arc<OnceLock<LetterAssembly>>,
}

impl Hook<InputEvent, InputVerdict> for FirstInputHook {
    fn call(
        &self,
        input: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let hook = self.clone();
        Box::pin(async move { hook.handle(input, cx).await })
    }
}

/// Forgets a closed session's text-only notice state.
#[derive(Debug, Clone)]
struct ForgetSession(Arc<Mutex<HashSet<SessionId>>>);

impl ObserveHook<SessionEnd> for ForgetSession {
    fn call(&self, input: SessionEnd, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&input.session);
        Box::pin(std::future::ready(Ok(())))
    }
}

impl FirstInputHook {
    /// Runs [`Self::first_input`] and, on a failure, publishes the failure
    /// text as one `letter` notice before the host's fixed `hook input
    /// failed` notice, which carries no cause.
    async fn handle(self, input: InputEvent, cx: HookCx) -> Result<InputVerdict, HookError> {
        let verdict = self.first_input(input, &cx).await;
        if let Err(HookError::Failed { message }) = &verdict {
            notify(&cx, not_persisted_notice(message));
        }
        verdict
    }

    /// Draws the letter assembly once per loaded registry.
    fn drawn(&self) -> &LetterAssembly {
        self.memo
            .get_or_init(|| letters(&self.registry, &self.font).unwrap_or_default())
    }

    /// Persists any missing tail of the first-prompt letter assembly and
    /// delivers the admitted images ahead of the running input.
    ///
    /// Stored records say what exists; delivery markers say what reached an
    /// admitted request. An empty store draws, persists, and attaches the
    /// whole assembly. A stored set that positionally matches is a strict
    /// prefix with two readings: a partial batch, which never produced a
    /// transform and so was never delivered, is completed and attached; a
    /// delivered prefix whose registry grew afterwards stays frozen, so the
    /// frozen set never changes and no delivered image is sent twice. A full
    /// positional match with no marker means the opening was dropped after
    /// persisting, so the images are attached without persisting again.
    /// Any other stored set (a mismatch, extra or duplicate rows) wins and
    /// freezes. A model that is not known to read images stores and attaches
    /// nothing. Records land before the returned `Transform`, so the model
    /// never sees an image without its record.
    ///
    /// # Errors
    ///
    /// Returns [`HookError::Cancelled`] when the turn is cancelled and the
    /// service text when a record read, blob write, or record write fails,
    /// or when a stored row is well-formed but outside the admitted bounds
    /// (the fold keeps the running content with one notice; a failed write
    /// leaves a prefix the next turn completes, while a corrupt row fails
    /// every turn until it is fixed). A font failure degrades to an
    /// empty assembly, so every description stays text.
    async fn first_input(&self, input: InputEvent, cx: &HookCx) -> Result<InputVerdict, HookError> {
        if cx.cancel.is_cancelled() {
            return Err(HookError::Cancelled);
        }
        let InputEvent { content, model } = input;
        let marked = self
            .registry
            .iter()
            .filter(|skill| skill.letter2image)
            .count();
        if content.is_empty() || marked == 0 {
            return Ok(InputVerdict::Continue);
        }
        if !model.as_ref().is_some_and(|model| model.caps.image_input) {
            let stored = stored_letters(cx).await?;
            let text = if model.is_some() {
                no_image_model_notice()
            } else {
                unknown_model_notice()
            };
            return Ok(self.skip(cx, &stored, text));
        }
        let stored = stored_letters(cx).await?;
        let assembly = self.drawn();
        if assembly.chunks.is_empty() {
            if stored.is_empty() {
                log_fallbacks(assembly);
            }
            return Ok(InputVerdict::Continue);
        }
        if stored.is_empty() {
            log_fallbacks(assembly);
            persist_tail(cx, &assembly.chunks).await?;
            return Ok(transform(assembly, content));
        }
        let matched = matched_prefix(&stored, assembly);
        if stored.len() > matched {
            return Ok(InputVerdict::Continue);
        }
        // Past the mismatch freeze `matched` equals `stored.len()`: a full
        // match re-attaches without persisting, a strict prefix completes
        // its tail first, and a delivered set in either shape freezes.
        if delivered(cx, &stored).await? {
            return Ok(InputVerdict::Continue);
        }
        if matched < assembly.chunks.len() {
            persist_tail(cx, &assembly.chunks[matched..]).await?;
        }
        Ok(transform(assembly, content))
    }

    /// Passes the input through as plain text. A session with no stored set
    /// is told why once; a stored set needs no explanation.
    fn skip(&self, cx: &HookCx, stored: &[SkillLetterRecord], text: &str) -> InputVerdict {
        if stored.is_empty()
            && self
                .told
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(cx.session)
        {
            notify(cx, text.to_owned());
        }
        InputVerdict::Continue
    }
}

/// Returns the admitted images followed by the running parts, so the folded
/// `User` record and the provider request carry the same final parts.
fn transform(assembly: &LetterAssembly, content: Vec<Part>) -> InputVerdict {
    let mut parts = image_parts(&assembly.chunks);
    parts.extend(content);
    InputVerdict::Transform(parts)
}

/// Reports whether every stored letter's image reached an admitted request:
/// each stored PNG digest appears in this leaf path's delivery markers. The
/// actor journals one marker in the same batch as any admitted user entry
/// that carries image parts, so markers never exist for a dropped opening.
async fn delivered(cx: &HookCx, stored: &[SkillLetterRecord]) -> Result<bool, HookError> {
    let bodies = cx
        .services
        .records(&cx.caller, DELIVERED_KIND)
        .await
        .map_err(failed)?;
    let mut digests = Vec::new();
    for body in &bodies {
        let Ok(marker) = body.decode_as::<LetterDelivered>() else {
            continue;
        };
        if marker.v != 1 {
            continue;
        }
        digests.extend(marker.digests);
    }
    Ok(stored
        .iter()
        .all(|record| digests.contains(&record.png_blob)))
}

/// Publishes one `letter` notice on the hook's turn.
fn notify(cx: &HookCx, text: String) {
    cx.services.notify(
        &cx.caller,
        Notice {
            turn: cx.turn,
            kind: "letter".into(),
            text: text.into(),
        },
    );
}

/// Logs one diagnostic per marked skill whose description stays plain text.
fn log_fallbacks(assembly: &LetterAssembly) {
    for fallback in &assembly.fallbacks {
        let notice = match fallback.reason {
            FallbackReason::Undrawable => undrawable_notice(
                &fallback.skill,
                &fallback.plugin,
                fallback.first_undrawable.unwrap_or(0),
            ),
            FallbackReason::OverBudget => over_budget_notice(&fallback.skill, &fallback.plugin),
            FallbackReason::RenderFailure => {
                render_failed_notice(&fallback.skill, &fallback.plugin)
            }
        };
        tracing::info!("{notice}");
    }
}

/// Maps any service, serialization, or record failure to its hook error.
fn failed(error: impl std::fmt::Display) -> HookError {
    HookError::Failed {
        message: error.to_string().into(),
    }
}

/// Reads this session's stored letter records: decoded, validated, and in
/// ascending id order. A body that does not decode fails the read, and so
/// does a well-formed body outside the admitted bounds: the failure lands
/// before any blob or record write, so a corrupt row can never hide behind
/// a freshly persisted duplicate id. Valid rows are preserved untouched;
/// nothing is migrated or deleted.
async fn stored_letters(cx: &HookCx) -> Result<Vec<SkillLetterRecord>, HookError> {
    let bodies = cx
        .services
        .records(&cx.caller, "letter")
        .await
        .map_err(failed)?;
    let mut admitted: Vec<(u8, SkillLetterRecord)> = Vec::with_capacity(bodies.len());
    for body in &bodies {
        let record = decode_record(body).map_err(failed)?;
        let Some(number) = record.validated() else {
            return Err(failed(format!(
                "letter record '{}' failed validation",
                record.id
            )));
        };
        admitted.push((number, record));
    }
    admitted.sort_by_key(|(number, _)| *number);
    Ok(admitted.into_iter().map(|(_, record)| record).collect())
}

/// Counts the stored strict prefix of `assembly`: records matched
/// positionally on identity, geometry, and source digest. The first gap,
/// duplicate, or mismatch ends the prefix; any stored row beyond it makes
/// the stored set a mismatch, which the caller freezes instead of extending.
fn matched_prefix(stored: &[SkillLetterRecord], assembly: &LetterAssembly) -> usize {
    stored
        .iter()
        .zip(&assembly.chunks)
        .take_while(|(record, chunk)| record_matches_chunk(record, chunk))
        .count()
}

/// Reports whether a stored record carries exactly `chunk`: same identity,
/// geometry, byte length, and source digest. The source digest implies the
/// PNG bytes (`draw` is pure over font and source), so the frozen path skips
/// rehashing megabyte images on every turn.
fn record_matches_chunk(record: &SkillLetterRecord, chunk: &LetterChunk) -> bool {
    record.id.as_ref() == chunk.id.as_ref()
        && record.skill.as_ref() == chunk.skill.as_ref()
        && record.plugin.as_ref() == chunk.plugin.as_ref()
        && record.png_bytes == u64::try_from(chunk.png.len()).unwrap_or(u64::MAX)
        && record.width == chunk.width
        && record.height == chunk.height
        && record.cell == chunk.cell
        && record.source_blob == BlobId::from_bytes(chunk.source_text.as_bytes())
}

/// Persists the missing tail of the letter set in two phases: every PNG and
/// source blob and every strict record body first, then the records in one
/// uninterrupted run.
///
/// Blobs are content-addressed and harmless without a record, so a
/// cancellation or failure while they are written leaves no record: the set
/// stays `Empty` and the next input repeats the idempotent puts. The records
/// are the commit point. Each future resolves only once durable, and
/// cancellation is checked once before the first record and never between
/// records, so a cancel cannot split the set. Only a journal failure or a
/// crash inside the run leaves a strict prefix, which the next input
/// completes from the persisted rows instead of duplicating ids.
async fn persist_tail(cx: &HookCx, tail: &[LetterChunk]) -> Result<(), HookError> {
    let mut bodies = Vec::with_capacity(tail.len());
    for chunk in tail {
        if cx.cancel.is_cancelled() {
            return Err(HookError::Cancelled);
        }
        let png_id = BlobId::from_bytes(chunk.png.as_ref());
        let source_id = BlobId::from_bytes(chunk.source_text.as_bytes());
        let stored_png = cx
            .services
            .blob_put(&cx.caller, chunk.png.as_ref().to_vec())
            .await
            .map_err(failed)?;
        if stored_png != *png_id.as_bytes() {
            return Err(HookError::Failed {
                message: "letter PNG blob digest mismatch".into(),
            });
        }
        let stored_source = cx
            .services
            .blob_put(&cx.caller, chunk.source_text.as_bytes().to_vec())
            .await
            .map_err(failed)?;
        if stored_source != *source_id.as_bytes() {
            return Err(HookError::Failed {
                message: "letter source blob digest mismatch".into(),
            });
        }
        let record = SkillLetterRecord {
            v: 1,
            id: chunk.id.clone(),
            kind: LetterKind::Skill,
            skill: chunk.skill.clone(),
            plugin: chunk.plugin.clone(),
            png_blob: png_id,
            source_blob: source_id,
            png_bytes: u64::try_from(chunk.png.len()).unwrap_or(u64::MAX),
            width: chunk.width,
            height: chunk.height,
            cell: chunk.cell,
        };
        let text = sonic_rs::to_string(&record).map_err(failed)?;
        bodies.push(RawValue::parse(&text).map_err(failed)?);
    }
    if cx.cancel.is_cancelled() {
        return Err(HookError::Cancelled);
    }
    for body in bodies {
        cx.services
            .append_record(&cx.caller, "letter", Box::new(body))
            .await
            .map_err(failed)?;
    }
    Ok(())
}

/// Builds the `letter` extension: the `letter` scheme plus the first-input
/// hook. Image blobs and letter records are persisted by the hook through
/// the host services seam (`blob_put`, `append_record`) only when the turn's
/// model reads images; the extension never touches journal or blob paths
/// directly.
///
/// # Errors
///
/// Returns the runtime's typed build error when the builder rejects the
/// registration.
pub fn extension(
    registry: Arc<crate::skills::SkillRegistry>,
) -> Result<Extension, RegistrationError> {
    let font = Arc::new(Font::embedded());
    let resolver = LetterResolver::new();
    let told = Arc::new(Mutex::new(HashSet::new()));
    let hook = FirstInputHook {
        registry,
        font,
        told: Arc::clone(&told),
        memo: Arc::new(OnceLock::new()),
    };
    ExtensionBuilder::new("letter", "0.1.0", ServiceSet::EMPTY)?
        .scheme("letter", Arc::new(resolver))
        .on_input(hook)
        .on_session_end_lossless(ForgetSession(told))
        .build()
}

/// First-input handling as a standalone entry point: the same handling the
/// `letter` extension's input hook runs, with fresh per-call notice and draw
/// state. Extension-driven turns should prefer the extension, whose hook
/// keeps the once-per-session notice state and the drawn assembly across
/// inputs; direct callers get one notice per call and draw once per call
/// instead.
///
/// It takes the whole [`InputEvent`] rather than bare parts because the
/// capability gate needs the turn's resolved model; callers without a
/// resolved route pass `model: None` and always take the text-only path,
/// never a blind attach.
///
/// # Errors
///
/// Returns [`HookError::Cancelled`] when the turn is cancelled.
/// Returns [`HookError::Failed`] if a record read, blob write, or record write fails.
pub async fn handle_first_input(
    session: SessionId,
    input: InputEvent,
    registry: Arc<crate::skills::SkillRegistry>,
    font: Arc<Font>,
    cx: HookCx,
) -> Result<InputVerdict, HookError> {
    debug_assert_eq!(session, cx.session);
    FirstInputHook {
        registry,
        font,
        told: Arc::new(Mutex::new(HashSet::new())),
        memo: Arc::new(OnceLock::new()),
    }
    .handle(input, cx)
    .await
}

#[cfg(test)]
mod hook_tests;
#[cfg(test)]
mod tests;
