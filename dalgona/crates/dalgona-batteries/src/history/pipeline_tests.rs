// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! History image pipeline behavior tests over a recording sink.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{BoxFuture, CompactError, Compaction, CoveredEntry};
use dal_core::{ContextItem, EntryId, Part, SessionId};
use dal_ext::Font;
use tokio::sync::Semaphore;

use super::CARRIED_PREFIX;
use super::pipeline::{
    Budget, Commit, Drawn, Engine, ImageProfile, Limits, Request, Slot, SourceReader,
};
use super::records::LetterRecord;
use super::spans::{CompactPiece, Role, SourceError, Span};

const TOKENS: u64 = 4761;

fn entry(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero entry"))
}

fn user(entry_no: u64, turn: bool, text: &str) -> CoveredEntry {
    CoveredEntry {
        entry: entry(entry_no),
        starts_user_turn: turn,
        estimated_tokens: 1_000_000,
        content: ContextItem::User {
            parts: vec![Part::Text { text: text.into() }],
        },
    }
}

fn covered() -> Vec<CoveredEntry> {
    vec![
        user(1, true, &"a".repeat(300)),
        user(2, true, &"b".repeat(40)),
    ]
}

fn profile(max_images: usize) -> ImageProfile {
    ImageProfile {
        cols: 17,
        rows: 1,
        cell_w: 8,
        cell_h: 16,
        max_images,
        image_tokens: TOKENS,
    }
}

fn budget(window: u64, share: f64) -> Budget {
    Budget {
        window_tokens: Some(window),
        total_tokens: 2_100_000,
        images_elsewhere: 0,
        image_bytes_elsewhere: 0,
        share,
    }
}

fn limits() -> Limits {
    Limits {
        render: Duration::from_secs(30),
        chain: Duration::from_secs(60),
        png_bytes: 3_000_000,
        savings: 0.9,
    }
}

#[derive(Debug, Default)]
struct FixtureSource {
    parts: HashMap<(u64, u32), Vec<u8>>,
}

impl SourceReader for FixtureSource {
    fn read(&self, span: Span) -> Result<Vec<u8>, SourceError> {
        let bytes = self
            .parts
            .get(&(span.entry.get(), span.part))
            .ok_or_else(|| SourceError {
                message: "fixture part missing".to_string(),
            })?;
        let start = usize::try_from(span.off).unwrap_or(usize::MAX);
        let end = start.saturating_add(usize::try_from(span.len).unwrap_or(usize::MAX));
        bytes
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| SourceError {
                message: "fixture range missing".to_string(),
            })
    }
}

fn pieces(entries: &[CoveredEntry]) -> (Vec<CompactPiece>, FixtureSource) {
    let mut pieces = Vec::new();
    let mut source = FixtureSource::default();
    for covered in entries {
        let ContextItem::User { parts } = &covered.content else {
            continue;
        };
        for (index, part) in parts.iter().enumerate() {
            let Part::Text { text } = part else {
                continue;
            };
            let part_index = u32::try_from(index).unwrap_or(u32::MAX);
            let length = u32::try_from(text.len()).unwrap_or(u32::MAX);
            pieces.push(CompactPiece {
                entry: covered.entry,
                part: part_index,
                off: 0,
                len: length,
                total: length,
                role: Role::User,
                picture: None,
            });
            source
                .parts
                .insert((covered.entry.get(), part_index), text.as_bytes().to_vec());
        }
    }
    (pieces, source)
}

fn request(entries: &[CoveredEntry], budget: Budget) -> Request {
    request_with_ordinal(entries, budget, 1)
}

fn request_with_ordinal(entries: &[CoveredEntry], budget: Budget, ordinal: u32) -> Request {
    let (pieces, source) = pieces(entries);
    Request::new(
        SessionId::new_v7(),
        entries,
        (entry(1), entry(2)),
        ordinal,
        budget,
        pieces,
        Arc::new(source),
    )
}

struct MissingSource;

impl SourceReader for MissingSource {
    fn read(&self, _span: Span) -> Result<Vec<u8>, SourceError> {
        Err(SourceError {
            message: "fixture source is unavailable".to_string(),
        })
    }
}

#[derive(Default)]
struct Recorder {
    drawn: Mutex<Option<Drawn>>,
}

impl Commit for Recorder {
    fn commit(
        &self,
        span: (EntryId, EntryId),
        drawn: Drawn,
    ) -> BoxFuture<'_, Result<Option<Compaction>, CompactError>> {
        *self.drawn.lock().expect("recorder lock") = Some(drawn);
        Box::pin(async move { Ok(Some(Compaction::text(span, "committed", None))) })
    }
}

fn engine(limits: Limits) -> Engine {
    Engine::new(
        Arc::new(Font::embedded()),
        Arc::new(Semaphore::new(4)),
        limits,
    )
}

async fn run(
    limits: Limits,
    profile: ImageProfile,
    request: Request,
) -> (Result<Option<Compaction>, CompactError>, Option<Drawn>) {
    let sink = Arc::new(Recorder::default());
    let result = engine(limits)
        .run(request, profile, Arc::clone(&sink) as Arc<dyn Commit>)
        .await;
    let drawn = sink.drawn.lock().expect("recorder lock").take();
    (result, drawn)
}

fn is_refusal(result: &Result<Option<Compaction>, CompactError>) -> bool {
    matches!(result, Err(CompactError::Fail(_)))
}

fn positions(drawn: &Drawn) -> Vec<usize> {
    drawn
        .letters
        .iter()
        .map(|letter| {
            let LetterRecord::Compaction { id, .. } = &letter.record else {
                panic!("history letters are compaction records");
            };
            let (_, index) = id.split_once('.').expect("ordinal.index id");
            index.parse::<usize>().expect("numeric index") - 1
        })
        .collect()
}

async fn all_letters() -> usize {
    let (result, drawn) = run(
        limits(),
        profile(1000),
        request(&covered(), budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert!(result.expect("unbounded run succeeds").is_some());
    drawn.expect("drawn").letters.len()
}

#[tokio::test]
async fn rendered_image_spans_resolve_in_the_source_fixture() {
    let entries = covered();
    let request = request(&entries, budget(u64::MAX / 4, 0.7));
    let source = request.source();
    let (result, drawn) = run(limits(), profile(1000), request).await;
    assert!(result.expect("run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    assert!(drawn.letters.len() > 2);
    let mut images = 0;
    for (index, slot) in drawn.slots.iter().enumerate() {
        let Slot::Image(letter) = slot else { continue };
        images += 1;
        let LetterRecord::Compaction { id, spans, .. } = &drawn.letters[*letter].record else {
            panic!("compaction record expected");
        };
        assert_eq!(
            drawn.slots[index - 1],
            Slot::Text(format!("letter://{id}").into())
        );
        assert!(
            drawn.letters[*letter]
                .index_line
                .starts_with(&format!("letter://{id}  "))
        );
        assert!(!spans.is_empty());
        for span in spans {
            let bytes = source
                .read(*span)
                .expect("span names readable source bytes");
            assert_eq!(bytes.len(), usize::try_from(span.len).expect("span length"));
            assert!(String::from_utf8(bytes).is_ok());
        }
        assert_eq!(
            &drawn.letters[*letter].png[..8],
            &[137, 80, 78, 71, 13, 10, 26, 10]
        );
    }
    assert_eq!(images, drawn.letters.len());
}

#[tokio::test]
async fn share_cap_keeps_oldest_plus_newest_at_the_exact_boundary() {
    let total = all_letters().await;
    assert!(total >= 12);
    let kept = 5_u64;
    let window = TOKENS * kept * 2;
    let (result, drawn) = run(
        limits(),
        profile(1000),
        request(&covered(), budget(window, 0.5)),
    )
    .await;
    assert!(result.expect("boundary run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    let mut expected = vec![0];
    expected.extend(total - 4..total);
    assert_eq!(positions(&drawn), expected);
    let (_, below) = run(
        limits(),
        profile(1000),
        request(&covered(), budget(window - 1, 0.5)),
    )
    .await;
    let below = below.expect("drawn below the boundary");
    let mut expected = vec![0];
    expected.extend(total - 3..total);
    assert_eq!(positions(&below), expected);
    let hidden = drawn
        .slots
        .iter()
        .rev()
        .find_map(|slot| match slot {
            Slot::Text(text) => Some(text.clone()),
            Slot::Image(_) => None,
        })
        .expect("index text is last");
    assert!(hidden.contains("letter://history/1.2 to letter://history/1."));
    assert!(hidden.starts_with(&format!(
        "{} of {total} history images",
        drawn.letters.len()
    )));
}

#[tokio::test]
async fn image_slots_are_the_catalog_maximum_minus_images_elsewhere() {
    let mut carried = budget(u64::MAX / 4, 0.7);
    carried.images_elsewhere = 1;
    let (result, drawn) = run(limits(), profile(3), request(&covered(), carried)).await;
    assert!(result.expect("run succeeds").is_some());
    assert_eq!(drawn.expect("drawn").letters.len(), 2);
    carried.images_elsewhere = 3;
    let (result, drawn) = run(limits(), profile(3), request(&covered(), carried)).await;
    assert!(is_refusal(&result));
    assert!(drawn.is_none());
}

#[tokio::test]
async fn png_budget_boundary_and_first_image_refusal() {
    let total = all_letters().await;
    let (_, drawn) = run(
        limits(),
        profile(1000),
        request(&covered(), budget(u64::MAX / 4, 0.7)),
    )
    .await;
    let sizes: Vec<usize> = drawn
        .expect("drawn")
        .letters
        .iter()
        .map(|letter| letter.png.len())
        .collect();
    let sum: usize = sizes.iter().sum();
    let mut exact = limits();
    exact.png_bytes = sum;
    let (_, fits) = run(
        exact,
        profile(1000),
        request(&covered(), budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert_eq!(fits.expect("drawn").letters.len(), total);
    exact.png_bytes = sum - 1;
    let (_, short) = run(
        exact,
        profile(1000),
        request(&covered(), budget(u64::MAX / 4, 0.7)),
    )
    .await;
    let short = short.expect("drawn");
    assert_eq!(
        positions(&short),
        std::iter::once(0).chain(2..total).collect::<Vec<_>>()
    );
    exact.png_bytes = 1;
    let (result, drawn) = run(
        exact,
        profile(1000),
        request(&covered(), budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert!(is_refusal(&result));
    assert!(drawn.is_none());
}

#[tokio::test]
async fn savings_factor_boundary_and_unprofitable_refusal() {
    let cheap = ImageProfile {
        image_tokens: 100,
        ..profile(1000)
    };
    let mut entries = covered();
    entries[0].estimated_tokens = 1000;
    entries[1].estimated_tokens = 0;
    let (result, drawn) = run(
        limits(),
        cheap,
        request(&entries, budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert!(result.expect("run succeeds").is_some());
    assert_eq!(drawn.expect("drawn").letters.len(), 9);
    entries[0].estimated_tokens = 999;
    let (_, drawn) = run(
        limits(),
        cheap,
        request(&entries, budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert_eq!(drawn.expect("drawn").letters.len(), 8);
    entries[0].estimated_tokens = 10;
    let (result, drawn) = run(
        limits(),
        cheap,
        request(&entries, budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert!(matches!(result, Err(CompactError::Fail(_))));
    assert!(drawn.is_none());
}

#[tokio::test]
async fn missing_journal_bytes_decline_before_commit() {
    let entries = covered();
    let (pieces, _) = pieces(&entries);
    let request = Request::new(
        SessionId::new_v7(),
        &entries,
        (entry(1), entry(2)),
        1,
        budget(u64::MAX / 4, 0.7),
        pieces,
        Arc::new(MissingSource),
    );
    let (result, drawn) = run(limits(), profile(10), request).await;
    assert!(matches!(result, Err(CompactError::Fail(_))));
    assert!(drawn.is_none());
}

#[tokio::test]
async fn declines_without_drawable_history_or_token_room() {
    let one_turn = vec![user(1, true, "hello"), user(2, false, "world")];
    let (result, drawn) = run(
        limits(),
        profile(10),
        request(&one_turn, budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert!(is_refusal(&result));
    assert!(drawn.is_none());
    let mut unknown = budget(1000, 0.4);
    unknown.window_tokens = None;
    let (result, _) = run(limits(), profile(10), request(&covered(), unknown)).await;
    assert!(is_refusal(&result));
    let (result, drawn) = run(
        limits(),
        profile(10),
        request(&covered(), budget(1000, 0.1)),
    )
    .await;
    assert!(is_refusal(&result));
    assert!(drawn.is_none());
}

#[tokio::test]
async fn timeouts_refuse_with_the_exact_notices_and_commit_nothing() {
    let entries = vec![user(1, true, &"z".repeat(3000)), user(2, true, "tail")];
    let mut render = limits();
    render.render = Duration::ZERO;
    let (result, drawn) = run(
        render,
        profile(1000),
        request(&entries, budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert!(is_refusal(&result));
    assert!(drawn.is_none());
    let mut chain = limits();
    chain.chain = Duration::ZERO;
    let (result, drawn) = run(
        chain,
        profile(1000),
        request(&entries, budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert!(is_refusal(&result));
    assert!(drawn.is_none());
}

#[tokio::test]
async fn undrawable_letter_is_shown_as_its_exact_text_without_an_image() {
    let entries = vec![user(1, true, "before"), user(2, true, "smile \u{1F600}")];
    let (result, drawn) = run(
        limits(),
        profile(10),
        request(&entries, budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert!(result.expect("run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    let text = drawn
        .slots
        .iter()
        .find_map(|slot| match slot {
            Slot::Text(text) if text.contains("is shown as text because") => Some(text.clone()),
            _ => None,
        })
        .expect("the undrawable letter is shown as text");
    assert!(text.contains("smile \u{1F600}"));
    assert!(text.contains("=== entry 2, user, part 0, bytes 0-"));
    assert!(text.ends_with('\n'));
    for position in positions(&drawn) {
        assert!(!text.starts_with(&format!("letter://history/1.{} ", position + 1)));
    }
    for letter in &drawn.letters {
        assert!(!letter.png.is_empty());
    }
}

#[tokio::test]
async fn carried_summary_follows_the_header_and_ids_use_the_ordinal() {
    let entries = covered();
    let carried = request(&entries, budget(u64::MAX / 4, 0.7)).with_carried("S".into());
    let (result, drawn) = run(limits(), profile(1000), carried).await;
    assert!(result.expect("run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    assert_eq!(
        drawn.slots[1],
        Slot::Text(format!("{CARRIED_PREFIX}S").into())
    );
    let mut third = request_with_ordinal(&entries, budget(u64::MAX / 4, 0.7), 3);
    third = third.with_carried("S".into());
    let (_, drawn) = run(limits(), profile(1000), third).await;
    let drawn = drawn.expect("drawn");
    for letter in &drawn.letters {
        let LetterRecord::Compaction { id, .. } = &letter.record else {
            panic!("compaction record expected");
        };
        assert!(id.starts_with("history/3."));
    }
}

#[test]
fn compaction_ordinal_reads_only_compaction_ids() {
    let compaction = LetterRecord::Compaction {
        v: 1,
        id: "history/7.2".to_string(),
        png_blob: "a".repeat(64),
        png_bytes: 1,
        width: 1,
        height: 1,
        cell: [1, 1],
        spans: Vec::new(),
        letters: Vec::new(),
    };
    assert_eq!(compaction.compaction_ordinal(), Some(7));
    let dream = LetterRecord::Dream {
        v: 1,
        id: "dream/2".to_string(),
        letters: Vec::new(),
        summary: String::new(),
    };
    assert_eq!(dream.compaction_ordinal(), None);
}

#[tokio::test]
async fn retained_image_bytes_use_the_png_budget() {
    let total = all_letters().await;
    let (_, drawn) = run(
        limits(),
        profile(1000),
        request(&covered(), budget(u64::MAX / 4, 0.7)),
    )
    .await;
    let sum: usize = drawn
        .expect("drawn")
        .letters
        .iter()
        .map(|letter| letter.png.len())
        .sum();
    let mut exact = limits();
    exact.png_bytes = sum;

    let mut carried = budget(u64::MAX / 4, 0.7);
    carried.image_bytes_elsewhere = 1;
    let (_, short) = run(exact, profile(1000), request(&covered(), carried)).await;
    assert_eq!(
        positions(&short.expect("drawn")),
        std::iter::once(0).chain(2..total).collect::<Vec<_>>()
    );

    carried.image_bytes_elsewhere = sum;
    let (result, drawn) = run(exact, profile(1000), request(&covered(), carried)).await;
    assert!(is_refusal(&result));
    assert!(drawn.is_none());

    carried.image_bytes_elsewhere = usize::MAX;
    let (result, drawn) = run(exact, profile(1000), request(&covered(), carried)).await;
    assert!(is_refusal(&result));
    assert!(drawn.is_none());
}
