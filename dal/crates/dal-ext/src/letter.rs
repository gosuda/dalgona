//! Deterministic rendering of text with the bundled bitmap font.
//!
//! Glyph data: GNU Unifont 18.0.01 (`unifont-18.0.01.hex`), used under the
//! SIL Open Font License, Version 1.1 (full text in `OFL-1.1.txt`).
//! Upstream: <https://unifoundry.com/unifont>/
//! Copyright (C) 1998-2026 Roman Czyborra, Paul Hardy, Qianqian Fang, Andrew Miller, Johnnie Weaver, David Corbett, Ælla Chiana Moskopp, Rebecca Bettencourt, Minseo Lee, Ho-Seok Ee, et al.
use std::sync::Arc;

use dal_agent::error::SchemeError;
use dal_agent::ext::{
    BoxFuture, Doc, Extension, ExtensionBuilder, Hook, HookCx, HookError, RawValue, SchemeCx,
    SchemeResolver,
};
use dal_core::{BlobId, InputEvent, InputVerdict, Part, RegistrationError, ServiceSet, SessionId};
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

/// First-input hook: validates the letter assembly is computable and returns
/// the input unchanged.
///
/// Blob and record persistence belong to the host actor, which calls the
/// pure [`letters`], [`crate::skills::section`], and [`image_parts`] seams
/// once for the first user prompt and withholds the provider request until
/// its store batch is durable. This hook therefore emits the fallback
/// diagnostics and returns [`InputVerdict::Continue`], keeping the running
/// content; it never returns image parts for blobs it cannot persist.
#[derive(Debug, Clone)]
struct FirstInputHook {
    registry: Arc<crate::skills::SkillRegistry>,
    font: Arc<Font>,
}

impl Hook<InputEvent, InputVerdict> for FirstInputHook {
    fn call(
        &self,
        input: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let registry = Arc::clone(&self.registry);
        let font = Arc::clone(&self.font);
        Box::pin(
            async move { handle_first_input(cx.session, input.content, registry, font, cx).await },
        )
    }
}

/// Validates the first-prompt letter assembly, emits fallback diagnostics,
/// and leaves the input unchanged for host-owned persistence.
///
/// Returns [`InputVerdict::Continue`]: there are no image parts to add, so
/// there is no content to replace. A future capture step with a blob seam
/// would return `Transform` with admitted images followed by these parts.
///
/// # Errors
///
/// Returns [`HookError::Cancelled`] when the turn is cancelled and
/// [`HookError::Failed`] when the font table fails to parse.
pub async fn handle_first_input(
    session: SessionId,
    user_parts: Vec<Part>,
    registry: Arc<crate::skills::SkillRegistry>,
    font: Arc<Font>,
    cx: HookCx,
) -> Result<InputVerdict, HookError> {
    debug_assert_eq!(session, cx.session);
    let _ = user_parts;
    if cx.cancel.is_cancelled() {
        return Err(HookError::Cancelled);
    }
    let assembly = match letters(&registry, &font) {
        Ok(assembly) => assembly,
        Err(error) => {
            return Err(HookError::Failed {
                message: error.to_string().into(),
            });
        }
    };
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
    Ok(InputVerdict::Continue)
}

/// Builds the `letter` extension: the `letter` scheme plus the first-input
/// hook. Image blobs and letter records are persisted by the host actor
/// through the pure assembly seams; this extension never opens the store.
///
/// # Errors
///
/// Returns the runtime's typed build error when the builder rejects the
/// registration.
pub fn extension() -> Result<Extension, RegistrationError> {
    let registry = Arc::new(crate::skills::SkillRegistry::empty());
    let font = Arc::new(Font::embedded());
    let resolver = LetterResolver::new();
    let hook = FirstInputHook { registry, font };
    ExtensionBuilder::new("letter", "0.1.0", ServiceSet::EMPTY)?
        .scheme("letter", Arc::new(resolver))
        .on_input(hook)
        .build()
}

#[cfg(test)]
mod tests;
