// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The history image pipeline: source pieces, drawing, budgets, and parts.
//!
//! [`Engine::run`] turns one covered span into a [`Drawn`] value and hands it
//! to a [`Commit`] sink. Every declined contract surfaces as a [`Decline`]
//! whose display text is the exact refusal notice, so the chain can fall
//! through to the text summary.

use std::sync::Arc;
use std::time::Duration;

use dal_agent::ext::{BoxFuture, CompactError, Compaction, CoveredEntry};
pub(crate) use dal_agent::ext::ImageProfile;
use dal_core::{EntryId, SessionId};
use dal_ext::Font;
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::time::timeout;

use super::WHOLE_CHAIN_TIMEOUT_S;
use super::draw::{DrawError, Grid, draw, paginate};
use super::records::{LetterRecord, RecordError};
use super::selection::{
    LetterVisibility, entry_id, history_index_line, index_text, select_oldest_plus_newest,
};
use super::spans::{CompactPiece, HistoryError, Item, Role, SourceError, Span, items};
use super::{CARRIED_PREFIX, HISTORY_HEADER, PNG_BYTE_BUDGET, RENDER_TIMEOUT_MS, SAVINGS_FACTOR};

/// Minimum completed user turns in the covered span for a drawable history.
const MIN_DRAWABLE_USER_TURNS: usize = 2;

/// Most hidden-letter groups named in the index text.
const MAX_HIDDEN_GROUPS: usize = 20;

/// A refused contract: the display text is the exact refusal notice.
#[derive(Debug, Error)]
pub(crate) enum Decline {
    /// The model has no complete catalog image profile.
    #[error("history: the model does not read images.")]
    NoImageProfile,
    /// The covered span holds nothing drawable.
    #[error("history: no new history to draw.")]
    NothingToDraw,
    /// The model context window is unknown, so no share can be computed.
    #[error("history: the model context window is unknown.")]
    UnknownWindow,
    /// The images would not cost fewer tokens than the text they replace.
    #[error("history: the images would cost {cost} tokens for {text} tokens of text.")]
    Unprofitable {
        /// Token bill of the images that survived earlier caps.
        cost: u64,
        /// Measured text tokens of the replaced span.
        text: u64,
    },
    /// The image share leaves no room for the first image.
    #[error(
        "history: no room for history images: {stay} of {window} tokens stay after compaction."
    )]
    NoTokenRoom {
        /// Tokens that stay in the request after compaction.
        stay: u64,
        /// The model context window.
        window: u64,
    },
    /// The request already carries the catalog maximum of images.
    #[error(
        "history: no room for history images: the request already carries {carried} of {max} images."
    )]
    NoSlot {
        /// Images already elsewhere in the request.
        carried: usize,
        /// Catalog maximum images per request.
        max: usize,
    },
    /// The PNG byte budget cannot fit the first image.
    #[error("history: the images need {need} bytes; the limit is {limit}.")]
    PngBudget {
        /// Encoded bytes of the first candidate image.
        need: usize,
        /// The configured byte budget.
        limit: usize,
    },
    /// Drawing exceeded the per-render timeout.
    #[error("history: drawing history images timed out after {millis} ms.")]
    RenderTimeout {
        /// The exceeded limit in milliseconds.
        millis: u128,
    },
    /// The whole chain exceeded its deadline.
    #[error("history: compaction timed out after {secs} s; falling back to text summary.")]
    ChainTimeout {
        /// The exceeded limit in seconds.
        secs: u64,
    },
    /// The host has no journal source for this covered span.
    #[error("history: the journal source is unavailable.")]
    SourceUnavailable,
    /// A journal source could not be read.
    #[error("history: {0}")]
    Source(#[from] HistoryError),
    /// A page failed to encode.
    #[error("history: {0}")]
    Draw(#[from] DrawError),
    /// A letter record failed structural validation.
    #[error("history: {0}")]
    Record(#[from] RecordError),
    /// The bundled font did not parse.
    #[error("history: the font could not be loaded.")]
    Font,
    /// The render worker did not finish.
    #[error("history: the render worker failed.")]
    Worker,
    /// The process-wide render gate closed.
    #[error("history: the render gate closed.")]
    GateClosed,
    /// The drawn parts disagree with their letters.
    #[error("history: the drawn images are inconsistent.")]
    Inconsistent,
    /// The host cannot commit image parts.
    #[error("history: this host cannot commit image parts.")]
    CannotCommit,
}

impl From<Decline> for CompactError {
    fn from(decline: Decline) -> Self {
        Self::fail(decline.to_string())
    }
}

/// Numbers the compaction part measured for one covered span.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    /// Model context window when known.
    pub(crate) window_tokens: Option<u64>,
    /// Total projected tokens of the branch.
    pub(crate) total_tokens: u64,
    /// Images already elsewhere in the request.
    pub(crate) images_elsewhere: usize,
    /// Fraction of the window billable to images.
    pub(crate) share: f64,
}

/// Reads source bytes for one exact journal span.
pub(crate) trait SourceReader: Send + Sync + 'static {
    /// Reads bytes from the journal span.
    ///
    /// # Errors
    /// Returns the source failure when the journal range is unavailable.
    fn read(&self, span: Span) -> Result<Vec<u8>, SourceError>;
}

/// One frozen compaction request.
pub(crate) struct Request {
    session: SessionId,
    span: (EntryId, EntryId),
    ordinal: u32,
    budget: Budget,
    carried: Option<Box<str>>,
    user_turns: usize,
    text_tokens: u64,
    pieces: Arc<[CompactPiece]>,
    source: Arc<dyn SourceReader>,
}

impl Request {
    /// Builds a request from the selected journal pieces.
    #[must_use]
    pub(crate) fn new(
        session: SessionId,
        covered: &[CoveredEntry],
        span: (EntryId, EntryId),
        ordinal: u32,
        budget: Budget,
        pieces: Vec<CompactPiece>,
        source: Arc<dyn SourceReader>,
    ) -> Self {
        Self {
            session,
            span,
            ordinal,
            budget,
            carried: None,
            user_turns: covered.iter().filter(|entry| entry.starts_user_turn).count(),
            text_tokens: covered
                .iter()
                .fold(0_u64, |sum, entry| sum.saturating_add(entry.estimated_tokens)),
            pieces: pieces.into(),
            source,
        }
    }

    /// Returns the request with an earlier summary carried in front of the images.
    #[must_use]
    pub(crate) fn with_carried(mut self, carried: Box<str>) -> Self {
        self.carried = Some(carried);
        self
    }

    /// Returns the journal source of this request.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn source(&self) -> Arc<dyn SourceReader> {
        Arc::clone(&self.source)
    }
}

/// One ordered part of the compacted message.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Slot {
    /// A text part.
    Text(Box<str>),
    /// The PNG of the letter at this index in [`Drawn::letters`].
    Image(usize),
}

/// One drawn letter: its PNG bytes and the record that names its source.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DrawnLetter {
    /// Encoded 1-bit PNG.
    pub(crate) png: Vec<u8>,
    /// The `letter` record to commit with the compaction.
    pub(crate) record: LetterRecord,
    /// The source index line to publish with the record.
    pub(crate) index_line: Box<str>,
}

/// The committed shape of one history compaction.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Drawn {
    /// Parts in message order.
    pub(crate) slots: Vec<Slot>,
    /// Letters whose images appear in `slots`.
    pub(crate) letters: Vec<DrawnLetter>,
    /// Image bill plus the text estimate of all text parts.
    pub(crate) parts_tokens: u64,
}

impl Drawn {
    fn verify(&self) -> Result<(), Decline> {
        let mut used = vec![false; self.letters.len()];
        let mut text_bytes = 0_usize;
        for slot in &self.slots {
            match slot {
                Slot::Text(text) => text_bytes = text_bytes.saturating_add(text.len()),
                Slot::Image(index) => match used.get_mut(*index) {
                    Some(seen) if !*seen => *seen = true,
                    _ => return Err(Decline::Inconsistent),
                },
            }
        }
        if used.iter().any(|seen| !seen) {
            return Err(Decline::Inconsistent);
        }
        let bill = u64::try_from(text_bytes.div_ceil(4)).map_err(|_| Decline::Inconsistent)?;
        if bill > self.parts_tokens {
            return Err(Decline::Inconsistent);
        }
        self.letters.iter().try_for_each(DrawnLetter::verify)
    }
}

impl DrawnLetter {
    fn verify(&self) -> Result<(), Decline> {
        let LetterRecord::Compaction {
            png_blob,
            png_bytes,
            ..
        } = &self.record
        else {
            return Err(Decline::Inconsistent);
        };
        let digest = blake3::hash(&self.png).to_hex();
        let same_size = usize::try_from(*png_bytes).is_ok_and(|size| size == self.png.len());
        let (first, last) = entry_range(self.record.spans());
        let id = self.record.id();
        let expected_index = history_index_line(id, first, last, LetterVisibility::Drawn);
        if digest.as_str() != png_blob
            || !same_size
            || self.record.spans().is_empty()
            || self.index_line.as_ref() != expected_index
        {
            return Err(Decline::Inconsistent);
        }
        Ok(())
    }
}

/// Receives one finished history compaction and commits it durably.
pub(crate) trait Commit: Send + Sync + 'static {
    /// Stores the PNG blobs, commits the letter records with the compaction,
    /// and returns the compaction, or refuses.
    fn commit(
        &self,
        span: (EntryId, EntryId),
        drawn: Drawn,
    ) -> BoxFuture<'_, Result<Option<Compaction>, CompactError>>;
}

/// Wall-clock and size limits of one compaction.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Per-render timeout.
    pub(crate) render: Duration,
    /// Whole-chain deadline.
    pub(crate) chain: Duration,
    /// Total encoded PNG bytes allowed.
    pub(crate) png_bytes: usize,
    /// Fraction of the replaced text tokens the image bill may reach.
    pub(crate) savings: f64,
}

impl Limits {
    /// The standard compaction limits.
    #[must_use]
    pub(crate) fn standard() -> Self {
        Self {
            render: Duration::from_millis(RENDER_TIMEOUT_MS),
            chain: Duration::from_secs(WHOLE_CHAIN_TIMEOUT_S),
            png_bytes: PNG_BYTE_BUDGET,
            savings: SAVINGS_FACTOR,
        }
    }
}

struct Candidate {
    items: Vec<Item>,
    spans: Vec<Span>,
    png: Option<Vec<u8>>,
}

/// Runs the image pipeline under the process-wide render gate.
pub(crate) struct Engine {
    font: Arc<Font>,
    permits: Arc<Semaphore>,
    limits: Limits,
}

impl Engine {
    /// Builds an engine over a shared font and render gate.
    #[must_use]
    pub(crate) fn new(font: Arc<Font>, permits: Arc<Semaphore>, limits: Limits) -> Self {
        Self {
            font,
            permits,
            limits,
        }
    }

    /// Draws and commits one history compaction.
    ///
    /// # Errors
    /// Returns the exact refusal text of the first unmet contract, the chain
    /// deadline, or the sink's own failure.
    pub(crate) async fn run(
        &self,
        request: Request,
        profile: ImageProfile,
        sink: Arc<dyn Commit>,
    ) -> Result<Option<Compaction>, CompactError> {
        let secs = self.limits.chain.as_secs();
        match timeout(
            self.limits.chain,
            self.draw_and_commit(request, profile, sink),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(Decline::ChainTimeout { secs }.into()),
        }
    }

    async fn draw_and_commit(
        &self,
        request: Request,
        profile: ImageProfile,
        sink: Arc<dyn Commit>,
    ) -> Result<Option<Compaction>, CompactError> {
        let permit = Arc::new(
            Arc::clone(&self.permits)
                .acquire_owned()
                .await
                .map_err(|_| Decline::GateClosed)?,
        );
        if request.user_turns < MIN_DRAWABLE_USER_TURNS || request.pieces.is_empty() {
            return Err(Decline::NothingToDraw.into());
        }
        let window = request.budget.window_tokens.ok_or(Decline::UnknownWindow)?;
        let mut candidates = self
            .render(&request, &profile, Arc::clone(&permit))
            .await?;
        let pool = drawable(&candidates);
        if pool.is_empty() {
            return Err(Decline::NothingToDraw.into());
        }
        let selected = self.select(&request, &profile, window, &candidates, pool)?;
        let drawn = assemble(&request, &profile, &mut candidates, &selected)?;
        drawn.verify()?;
        sink.commit(request.span, drawn).await
    }

    async fn render(
        &self,
        request: &Request,
        profile: &ImageProfile,
        permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<Vec<Candidate>, Decline> {
        let font = Arc::clone(&self.font);
        let source = Arc::clone(&request.source);
        let pieces = Arc::clone(&request.pieces);
        let grid = profile_grid(&profile);
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            render_pages(&font, grid, &pieces, source.as_ref())
        });
        match timeout(self.limits.render, worker).await {
            Err(_) => Err(Decline::RenderTimeout {
                millis: self.limits.render.as_millis(),
            }),
            Ok(Err(_)) => Err(Decline::Worker),
            Ok(Ok(rendered)) => rendered,
        }
    }

    fn select(
        &self,
        request: &Request,
        profile: &ImageProfile,
        window: u64,
        candidates: &[Candidate],
        pool: Vec<usize>,
    ) -> Result<Vec<usize>, Decline> {
        let tokens = profile.image_tokens;
        let share_cap = scaled(request.budget.share, window);
        let mut used = 0_u64;
        let pool = keep(&pool, |_| take(&mut used, tokens, share_cap));
        if pool.is_empty() {
            return Err(Decline::NoTokenRoom {
                stay: request
                    .budget
                    .total_tokens
                    .saturating_sub(request.text_tokens),
                window,
            });
        }
        let free = profile
            .max_images
            .saturating_sub(request.budget.images_elsewhere);
        let mut count = 0_usize;
        let pool = keep(&pool, |_| {
            count += 1;
            count <= free
        });
        if pool.is_empty() {
            return Err(Decline::NoSlot {
                carried: request.budget.images_elsewhere,
                max: profile.max_images,
            });
        }
        let first = png_len(&candidates[pool[0]]);
        let mut bytes = 0_usize;
        let pool = keep(&pool, |index| {
            let next = bytes.saturating_add(png_len(&candidates[index]));
            let fits = next <= self.limits.png_bytes;
            if fits {
                bytes = next;
            }
            fits
        });
        if pool.is_empty() {
            return Err(Decline::PngBudget {
                need: first,
                limit: self.limits.png_bytes,
            });
        }
        let saving_cap = scaled(self.limits.savings, request.text_tokens);
        let mut bill = 0_u64;
        let selected = keep(&pool, |_| take(&mut bill, tokens, saving_cap));
        if selected.is_empty() {
            let count = u64::try_from(pool.len()).unwrap_or(u64::MAX);
            return Err(Decline::Unprofitable {
                cost: tokens.saturating_mul(count),
                text: request.text_tokens,
            });
        }
        Ok(selected)
    }
}

fn take(used: &mut u64, tokens: u64, cap: u64) -> bool {
    let next = used.saturating_add(tokens);
    let fits = next <= cap;
    if fits {
        *used = next;
    }
    fits
}

fn profile_grid(profile: &ImageProfile) -> Grid {
    Grid {
        cols: profile.cols,
        rows: profile.rows,
        cell_w: profile.cell_w,
        cell_h: profile.cell_h,
    }
}

fn profile_width(profile: &ImageProfile) -> u32 {
    u32::from(profile.cols) * u32::from(profile.cell_w)
}

fn profile_height(profile: &ImageProfile) -> u32 {
    u32::from(profile.rows) * u32::from(profile.cell_h)
}

fn png_len(candidate: &Candidate) -> usize {
    candidate.png.as_ref().map_or(0, Vec::len)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the product of a finite non-negative factor and a token count is floored into u64"
)]
fn scaled(factor: f64, tokens: u64) -> u64 {
    let value = (factor * tokens as f64).floor();
    if value.is_finite() && value >= 0.0 {
        value as u64
    } else {
        0
    }
}

fn keep(pool: &[usize], mut fits: impl FnMut(usize) -> bool) -> Vec<usize> {
    select_oldest_plus_newest(pool.len(), |slot| fits(pool[slot]))
        .into_iter()
        .map(|slot| pool[slot])
        .collect()
}

fn drawable(candidates: &[Candidate]) -> Vec<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| candidate.png.is_some())
        .map(|(index, _)| index)
        .collect()
}

fn render_pages(
    font: &Font,
    grid: Grid,
    pieces: &[CompactPiece],
    source: &dyn SourceReader,
) -> Result<Vec<Candidate>, Decline> {
    let glyphs = font.glyphs().map_err(|_| Decline::Font)?;
    let built = items(pieces, |span| source.read(span))?;
    let mut out = Vec::new();
    for page in paginate(glyphs, grid, &built) {
        let spans = page_spans(&page.items);
        if spans.is_empty() {
            continue;
        }
        let png = if page.drawable {
            Some(draw(glyphs, grid, &page)?)
        } else {
            None
        };
        out.push(Candidate {
            items: page.items,
            spans,
            png,
        });
    }
    Ok(out)
}

fn page_spans(items: &[Item]) -> Vec<Span> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::Text { span, .. } | Item::Picture { span, .. } => Some(*span),
            Item::Mark(_) => None,
        })
        .collect()
}

fn entry_range(spans: &[Span]) -> (u64, u64) {
    let entries = spans.iter().map(|span| span.entry.get());
    let first = entries.clone().min().unwrap_or(0);
    (first, entries.max().unwrap_or(0))
}

fn assemble(
    request: &Request,
    profile: &ImageProfile,
    candidates: &mut [Candidate],
    selected: &[usize],
) -> Result<Drawn, Decline> {
    let next_value = request.span.1.get().checked_add(1).ok_or(Decline::Inconsistent)?;
    let next = entry_id(next_value).ok_or(Decline::Inconsistent)?;
    let mut slots = vec![Slot::Text(Box::from(HISTORY_HEADER))];
    if let Some(carried) = &request.carried {
        slots.push(Slot::Text(format!("{CARRIED_PREFIX}{carried}").into()));
    }
    let mut letters = Vec::new();
    let mut hidden = Vec::new();
    let mut shown_as_text = 0_usize;
    let total = candidates.len();
    for (position, candidate) in candidates.iter_mut().enumerate() {
        let index = position + 1;
        let id = format!("history/{}.{index}", request.ordinal);
        if selected.contains(&position) {
            slots.push(Slot::Text(format!("letter://{id}").into()));
            slots.push(Slot::Image(letters.len()));
            letters.push(letter(profile, candidate, &id, next)?);
        } else if candidate.png.is_none() {
            shown_as_text += 1;
            let text = letter_text(&id, request.session, &candidate.items);
            let lead = format!(
                "letter://{id} is shown as text because it holds characters that the font cannot draw:\n{text}"
            );
            slots.push(Slot::Text(lead.into()));
        } else {
            hidden.push((position, entry_range(&candidate.spans)));
        }
    }
    let shown = letters.len() + shown_as_text;
    slots.push(Slot::Text(
        index_text(shown, total, &hidden_ranges(request.ordinal, &hidden)).into(),
    ));
    let text_bytes: usize = slots
        .iter()
        .map(|slot| match slot {
            Slot::Text(text) => text.len(),
            Slot::Image(_) => 0,
        })
        .sum();
    let text_tokens = u64::try_from(text_bytes.div_ceil(4)).map_err(|_| Decline::Inconsistent)?;
    let images = u64::try_from(letters.len()).map_err(|_| Decline::Inconsistent)?;
    Ok(Drawn {
        slots,
        letters,
        parts_tokens: images
            .saturating_mul(profile.image_tokens)
            .saturating_add(text_tokens),
    })
}

fn letter(
    profile: &ImageProfile,
    candidate: &mut Candidate,
    id: &str,
    next: EntryId,
) -> Result<DrawnLetter, Decline> {
    let png = candidate.png.take().ok_or(Decline::Inconsistent)?;
    let record = LetterRecord::Compaction {
        v: 1,
        id: id.to_string(),
        png_blob: blake3::hash(&png).to_hex().to_string(),
        png_bytes: u32::try_from(png.len()).map_err(|_| Decline::Inconsistent)?,
        width: profile_width(profile),
        height: profile_height(profile),
        cell: [profile.cell_w, profile.cell_h],
        spans: candidate.spans.clone(),
        letters: Vec::new(),
    };
    LetterRecord::check(&record, next)?;
    let (first, last) = entry_range(&candidate.spans);
    let index_line = history_index_line(&record.id(), first, last, LetterVisibility::Drawn);
    Ok(DrawnLetter {
        png,
        record,
        index_line: index_line.into(),
    })
}

fn hidden_ranges(ordinal: u32, hidden: &[(usize, (u64, u64))]) -> String {
    let mut groups: Vec<(usize, usize, u64, u64)> = Vec::new();
    for (position, (first, last)) in hidden {
        match groups.last_mut() {
            Some(group) if group.1 + 1 == *position => {
                group.1 = *position;
                group.2 = group.2.min(*first);
                group.3 = group.3.max(*last);
            }
            _ => groups.push((*position, *position, *first, *last)),
        }
    }
    let named: Vec<String> = groups
        .iter()
        .take(MAX_HIDDEN_GROUPS)
        .map(|(from, to, first, last)| {
            if from == to {
                let id = format!("history/{ordinal}.{}", from + 1);
                format!("letter://{id} (entries {first}-{last})")
            } else {
                format!(
                    "letter://history/{ordinal}.{} to letter://history/{ordinal}.{} (entries {first}-{last})",
                    from + 1,
                    to + 1
                )
            }
        })
        .collect();
    let mut text = named.join(", ");
    if groups.len() > MAX_HIDDEN_GROUPS {
        text.push_str(&format!(
            "; and {} more ranges",
            groups.len() - MAX_HIDDEN_GROUPS
        ));
    }
    text
}

/// Renders the exact per-letter source text of one letter from its items.
#[must_use]
pub(crate) fn letter_text(id: &str, session: SessionId, items: &[Item]) -> String {
    let pieces: Vec<String> = items
        .iter()
        .filter_map(|item| match item {
            Item::Text {
                span,
                role,
                total,
                text,
            } => {
                let end = u64::from(span.off) + u64::from(span.len);
                let mut block = format!(
                    "=== entry {}, {}, part {}, bytes {}-{end} of {total}\n{text}",
                    span.entry.get(),
                    role.words(),
                    span.part,
                    span.off,
                );
                if !block.ends_with('\n') {
                    block.push('\n');
                }
                Some(block)
            }
            Item::Picture { span, mime, bytes } => Some(format!(
                "=== entry {}, image, part {}, {mime}, {bytes} bytes, not shown\n",
                span.entry.get(),
                span.part,
            )),
            Item::Mark(_) => None,
        })
        .collect();
    let noun = if pieces.len() == 1 { "piece" } else { "pieces" };
    format!(
        "letter://{id} holds {} {noun} of the journal of session {session}. Each piece starts with a line that begins with \"=== entry\". The exact text of the piece follows that line, and a line break ends a piece whose text does not end with one. \"bytes a-b\" means from byte a up to, but not including, byte b.\n{}",
        pieces.len(),
        pieces.concat()
    )
}
