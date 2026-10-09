// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! History image pipeline behavior tests over a recording sink.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::error::ServiceError;
use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    BoxFuture, Caller, CompactError, CompactInput, Compaction, Compactor, CoveredEntry, Doc,
    EventStream, RawValue, Services, Tool, ToolCx, ToolOutcome,
};
use dal_core::ext::{McpDeclaration, McpRequest, McpResponse, Visibility};
use dal_core::{
    AgentsOp, AgentsReply, Answer, AssistantPart, BlobId, CallId, ContextItem, EntryId, Family,
    FetchRequest, FetchResponse, Inference, JobsOp, JobsReply, ModelRequest, ModelRoute, Notice,
    Part, Question, RawJson, ReplaySource, RequestParams, RunOutput, RunRequest, SessionId,
    SidecarOp, StateError, StateOp, StateRecord, TurnOp, TurnOpReply,
};
use dal_ext::Font;
use tokio::sync::Semaphore;

use super::CARRIED_PREFIX;
use super::compact::{BlobStore, HistoryCompactor, ImageHost, PartsSink};
use super::pipeline::{
    Budget, Commit, Drawn, Engine, ImageProfile, KnownLetter, Limits, Request, Slot, SourceReader,
};
use super::records::{LetterRecord, journal_input, text_blobs};
use super::selection::LetterVisibility;
use super::spans::{CompactPiece, Item, Role, SourceError, Span, items};

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
        note: None,
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
        let line = drawn
            .index
            .iter()
            .find(|line| line.id.as_ref() == id.as_str())
            .expect("every drawn letter has an index line");
        assert_eq!(line.visibility, LetterVisibility::Drawn);
        assert!(line.text.starts_with(&format!("letter://{id}  ")));
        assert_ne!(spans.len(), 0);
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
        assert_ne!(letter.png.len(), 0);
    }
    let shown_as_text: Vec<_> = drawn
        .index
        .iter()
        .filter(|line| line.visibility == LetterVisibility::ShownAsText)
        .collect();
    assert_eq!(shown_as_text.len(), 1);
    assert!(shown_as_text[0].text.ends_with(", shown as text"));
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

/// Positions of the letters that the draw path left as text.
fn text_positions(drawn: &Drawn) -> Vec<usize> {
    drawn
        .slots
        .iter()
        .filter_map(|slot| match slot {
            Slot::Text(text) => text
                .strip_prefix("letter://history/1.")?
                .split_once(' ')
                .filter(|(_, rest)| rest.starts_with("is shown as text because"))
                .and_then(|(index, _)| index.parse::<usize>().ok())
                .map(|index| index - 1),
            Slot::Image(_) => None,
        })
        .collect()
}

#[tokio::test]
async fn index_names_every_letter_by_visibility_in_path_order() {
    let entries = vec![
        user(1, true, &"a".repeat(120)),
        user(2, true, &format!("smile \u{1F680} {}", "b".repeat(120))),
    ];
    let kept = 4_u64;
    let (result, drawn) = run(
        limits(),
        profile(1000),
        request(&entries, budget(TOKENS * kept * 2, 0.5)),
    )
    .await;
    assert!(result.expect("run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    let selected = positions(&drawn);
    let as_text = text_positions(&drawn);
    assert_eq!(selected.len(), 4);
    assert_ne!(as_text.len(), 0, "the rocket letter is shown as text");
    assert!(drawn.index.len() > selected.len() + as_text.len());

    for (position, line) in drawn.index.iter().enumerate() {
        assert_eq!(line.id.as_ref(), format!("history/1.{}", position + 1));
        let (visibility, suffix) = if selected.contains(&position) {
            (LetterVisibility::Drawn, "")
        } else if as_text.contains(&position) {
            (LetterVisibility::ShownAsText, ", shown as text")
        } else {
            (LetterVisibility::NotDrawn, ", not drawn")
        };
        assert_eq!(line.visibility, visibility, "letter {}", line.id);
        let entries = line
            .text
            .strip_prefix(&format!("letter://{}  history image, entries ", line.id))
            .and_then(|rest| rest.strip_suffix(suffix))
            .expect("the index line names the letter and ends with its visibility suffix");
        let (first, last) = entries.split_once('-').expect("entries a-b");
        assert!(first.parse::<u64>().expect("first entry") <= last.parse::<u64>().expect("last"));
    }

    let hidden: Vec<usize> = (0..drawn.index.len())
        .filter(|position| !selected.contains(position) && !as_text.contains(position))
        .collect();
    let index_text = drawn
        .slots
        .iter()
        .rev()
        .find_map(|slot| match slot {
            Slot::Text(text) => Some(text.clone()),
            Slot::Image(_) => None,
        })
        .expect("index text is last");
    assert!(index_text.contains(&format!("letter://history/1.{}", hidden[0] + 1)));
}

fn covered_entry(entry_no: u64, turn: bool, content: ContextItem) -> CoveredEntry {
    CoveredEntry {
        entry: entry(entry_no),
        starts_user_turn: turn,
        estimated_tokens: 1_000_000,
        content,
        note: None,
    }
}

fn assistant(parts: Vec<AssistantPart>) -> ContextItem {
    ContextItem::Assistant {
        source: ReplaySource {
            family: Family::Anthropic,
            model: "claude".into(),
        },
        parts,
    }
}

fn call(args: &str) -> AssistantPart {
    AssistantPart::ToolCall {
        call: CallId::new("c1"),
        name: "read".into(),
        args: RawJson::parse(args).expect("fixture JSON parses"),
    }
}

fn tool_result(name: &str, is_error: bool, parts: Vec<Part>) -> ContextItem {
    ContextItem::ToolResult {
        call: CallId::new("c1"),
        name: name.into(),
        is_error,
        parts,
    }
}

fn text(text: &str) -> Part {
    Part::Text { text: text.into() }
}

/// One entry per journal role, with images and a stored text blob.
fn every_role() -> Vec<CoveredEntry> {
    vec![
        covered_entry(
            1,
            true,
            ContextItem::User {
                parts: vec![
                    text("hello"),
                    Part::Image {
                        mime: "image/png".into(),
                        bytes: vec![1, 2, 3].into(),
                    },
                ],
            },
        ),
        covered_entry(
            2,
            false,
            assistant(vec![
                AssistantPart::Thinking {
                    text: "private plan".into(),
                    replay: None,
                },
                AssistantPart::Text {
                    text: "on it".into(),
                },
                call(r#"{"path":"a.rs"}"#),
            ]),
        ),
        covered_entry(
            3,
            false,
            tool_result("read", false, vec![text("file text")]),
        ),
        covered_entry(
            4,
            false,
            tool_result(
                "bash",
                true,
                vec![
                    text("boom"),
                    Part::Blob {
                        blob_id: BlobId::from_bytes(b"stored tail"),
                        mime: "text/plain".into(),
                        bytes: 11,
                    },
                ],
            ),
        ),
    ]
}

/// Serves `stored tail` for every stored text part of `covered`.
fn prefetch_blobs(covered: &[CoveredEntry]) -> HashMap<(u64, u32), Arc<[u8]>> {
    text_blobs(covered)
        .into_iter()
        .map(|(key, _)| (key, Arc::from(b"stored tail".to_vec())))
        .collect()
}

#[test]
fn journal_input_marks_every_role_and_keeps_journal_part_indexes() {
    let entries = every_role();
    let (pieces, source) =
        journal_input(&entries, &prefetch_blobs(&entries)).expect("journal input");
    let built = items(&pieces, |span| source.read(span)).expect("items build");
    let seen: Vec<String> = built
        .iter()
        .map(|item| match item {
            Item::Mark(mark) => format!("mark {mark}"),
            Item::Text {
                span, role, text, ..
            } => format!(
                "text {}:{} {} {text}",
                span.entry.get(),
                span.part,
                role.words()
            ),
            Item::Picture { span, mime, bytes } => {
                format!("picture {}:{} {mime} {bytes}", span.entry.get(), span.part)
            }
        })
        .collect();
    assert_eq!(
        seen,
        [
            "mark ¶user: ",
            "text 1:0 user hello",
            "picture 1:1 image/png 3",
            "mark ¶ai: ",
            "text 2:1 assistant on it",
            "mark ¶call:read ",
            "text 2:2 call read {\"path\":\"a.rs\"}",
            "mark ¶out:read ",
            "text 3:0 output read file text",
            "mark ¶failed:bash ",
            "text 4:0 failed output bash boom",
            "mark ¶failed:bash ",
            "text 4:1 failed output bash stored tail",
        ]
    );
}

#[test]
fn journal_source_returns_exact_bytes_and_names_what_is_missing() {
    let entries = every_role();
    let (_, source) = journal_input(&entries, &prefetch_blobs(&entries)).expect("journal input");
    let span = |entry_no: u64, part: u32, off: u32, len: u32| Span {
        entry: entry(entry_no),
        part,
        off,
        len,
    };
    assert_eq!(source.read(span(1, 0, 1, 3)).expect("inside"), b"ell");
    assert_eq!(
        source.read(span(2, 2, 0, 15)).expect("call arguments"),
        br#"{"path":"a.rs"}"#
    );
    assert_eq!(
        source.read(span(4, 1, 0, 11)).expect("stored text"),
        b"stored tail"
    );
    for missing in [
        span(9, 0, 0, 1),
        span(1, 0, 3, 10),
        span(2, 0, 0, 1),
        span(1, 0, u32::MAX, 2),
    ] {
        let error = source.read(missing).expect_err("the span has no bytes");
        assert!(
            error.message.contains(&format!("part {}", missing.part)),
            "{}",
            error.message
        );
    }
}

#[tokio::test]
async fn journal_input_drives_the_pipeline_and_names_roles_in_letter_text() {
    let entries = vec![
        covered_entry(
            1,
            true,
            ContextItem::User {
                parts: vec![text("hello")],
            },
        ),
        covered_entry(
            2,
            false,
            assistant(vec![
                AssistantPart::Text {
                    text: "on it".into(),
                },
                call("{\"path\":\"\u{1F680}.rs\"}"),
            ]),
        ),
        covered_entry(
            3,
            false,
            tool_result("read", false, vec![text("file text")]),
        ),
        covered_entry(
            4,
            true,
            ContextItem::User {
                parts: vec![text("again")],
            },
        ),
    ];
    let (pieces, source) =
        journal_input(&entries, &prefetch_blobs(&entries)).expect("journal input");
    let source: Arc<dyn SourceReader> = Arc::new(source);
    let request = Request::new(
        SessionId::new_v7(),
        &entries,
        (entry(1), entry(4)),
        1,
        budget(u64::MAX / 4, 0.7),
        pieces,
        Arc::clone(&source),
    );
    let (result, drawn) = run(limits(), profile(1000), request).await;
    assert!(result.expect("run succeeds").is_some());
    let drawn = drawn.expect("drawn");

    let mut drawn_bytes = Vec::new();
    for letter in &drawn.letters {
        for span in letter.record.spans() {
            let bytes = source.read(*span).expect("the journal serves every span");
            assert_eq!(bytes.len(), usize::try_from(span.len).expect("len"));
            drawn_bytes.extend(bytes);
        }
    }
    assert!(
        String::from_utf8(drawn_bytes)
            .expect("utf8")
            .ends_with("file textagain")
    );

    let as_text = drawn
        .slots
        .iter()
        .find_map(|slot| match slot {
            Slot::Text(text) if text.contains("is shown as text because") => Some(text.clone()),
            _ => None,
        })
        .expect("the rocket letter is shown as text");
    assert!(as_text.contains("=== entry 2, call read, part 1, bytes 0-"));
    assert!(
        drawn
            .index
            .iter()
            .any(|line| line.visibility == LetterVisibility::ShownAsText)
    );
}

#[test]
fn reminder_entries_become_note_pieces() {
    let mut reminder = covered_entry(5, false, ContextItem::User { parts: Vec::new() });
    reminder.note = Some("mind the gap".into());
    let (pieces, source) =
        journal_input(std::slice::from_ref(&reminder), &HashMap::new()).expect("journal input");
    let built = items(&pieces, |span| source.read(span)).expect("items build");
    assert_eq!(built[0], Item::Mark("¶note: ".into()));
    let Item::Text {
        span, role, text, ..
    } = &built[1]
    else {
        panic!("the reminder text becomes one text piece");
    };
    assert_eq!(
        (span.entry.get(), span.part, span.off, span.len),
        (5, 0, 0, 12)
    );
    assert_eq!(role.words(), "note");
    assert_eq!(text.as_ref(), "mind the gap");
    assert_eq!(
        source
            .read(Span {
                entry: entry(5),
                part: 0,
                off: 5,
                len: 3,
            })
            .expect("the note bytes are readable"),
        b"the"
    );
}

#[test]
fn stored_text_stays_text_and_a_missing_blob_declines_the_span() {
    let entries = vec![covered_entry(
        1,
        true,
        ContextItem::User {
            parts: vec![
                text("inline"),
                Part::Blob {
                    blob_id: BlobId::from_bytes(b"stored tail"),
                    mime: "text/plain".into(),
                    bytes: 11,
                },
            ],
        },
    )];
    assert_eq!(
        text_blobs(&entries),
        vec![((1, 1), BlobId::from_bytes(b"stored tail"))]
    );
    let (pieces, source) =
        journal_input(&entries, &prefetch_blobs(&entries)).expect("journal input");
    let built = items(&pieces, |span| source.read(span)).expect("items build");
    assert_eq!(
        built[1],
        Item::Text {
            span: Span {
                entry: entry(1),
                part: 0,
                off: 0,
                len: 6,
            },
            role: Role::User,
            total: 6,
            text: "inline".into(),
        }
    );
    assert_eq!(
        built[3],
        Item::Text {
            span: Span {
                entry: entry(1),
                part: 1,
                off: 0,
                len: 11,
            },
            role: Role::User,
            total: 11,
            text: "stored tail".into(),
        }
    );
    assert!(
        journal_input(&entries, &HashMap::new()).is_none(),
        "a stored text part that was not read declines the whole span"
    );
}

async fn drawn_unbounded(entries: &[CoveredEntry]) -> Drawn {
    let (result, drawn) = run(
        limits(),
        profile(1000),
        request(entries, budget(u64::MAX / 4, 0.7)),
    )
    .await;
    assert!(result.expect("unbounded run succeeds").is_some());
    drawn.expect("drawn")
}

fn stored_letters(drawn: &Drawn) -> Vec<KnownLetter> {
    drawn
        .letters
        .iter()
        .map(|letter| KnownLetter {
            record: letter.record.clone(),
            png: Some(letter.png.clone()),
        })
        .collect()
}

fn second_request(entries: &[CoveredEntry], known: Vec<KnownLetter>) -> Request {
    let (pieces, source) = pieces(entries);
    Request::new(
        SessionId::new_v7(),
        entries,
        (entry(1), entry(2)),
        2,
        budget(u64::MAX / 4, 0.7),
        pieces,
        Arc::new(source),
    )
    .with_known(known)
}

#[tokio::test]
async fn known_letters_are_reused_with_their_ids_and_pngs() {
    let entries = covered();
    let first = drawn_unbounded(&entries).await;
    let known = stored_letters(&first);
    assert!(known.len() > 2, "the fixture draws several letters");

    let (result, drawn) = run(limits(), profile(1000), second_request(&entries, known)).await;
    assert!(result.expect("second run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    assert_eq!(drawn.letters.len(), first.letters.len());
    for (left, right) in first.letters.iter().zip(&drawn.letters) {
        assert!(right.reused, "{} stays a stored letter", right.record.id());
        assert_eq!(left.record, right.record);
        assert_eq!(left.png, right.png);
        assert!(right.record.id().starts_with("history/1."));
    }
}

#[tokio::test]
async fn stale_known_letters_are_listed_and_redrawn_fresh() {
    let entries = covered();
    let first = drawn_unbounded(&entries).await;
    let mut known = stored_letters(&first);
    let LetterRecord::Compaction { id, cell, .. } = &mut known[1].record else {
        panic!("a compaction record");
    };
    let stale_id = id.clone();
    *cell = [1, 1];

    let (result, drawn) = run(limits(), profile(1000), second_request(&entries, known)).await;
    assert!(result.expect("second run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    assert!(
        drawn
            .letters
            .iter()
            .all(|letter| letter.record.id() != stale_id),
        "the stale letter is never shown as itself"
    );
    let stale_line = drawn
        .index
        .iter()
        .find(|line| line.id.as_ref() == stale_id)
        .expect("the stale letter stays listed");
    assert_eq!(stale_line.visibility, LetterVisibility::NotDrawn);
    assert!(stale_line.text.ends_with(", not drawn"));
    assert!(
        drawn
            .letters
            .iter()
            .any(|letter| letter.record.id().starts_with("history/2.")),
        "the stale content is drawn again under a fresh id"
    );
}

#[derive(Default)]
struct MapStore {
    blobs: Mutex<HashMap<[u8; 32], Vec<u8>>>,
    gets: AtomicUsize,
}

impl BlobStore for MapStore {
    fn put(&self, png: Vec<u8>) -> BoxFuture<'static, Result<[u8; 32], CompactError>> {
        let digest = *blake3::hash(&png).as_bytes();
        self.blobs.lock().expect("store lock").insert(digest, png);
        Box::pin(std::future::ready(Ok(digest)))
    }

    fn get(&self, digest: [u8; 32]) -> BoxFuture<'static, Result<Option<Vec<u8>>, CompactError>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let png = self.blobs.lock().expect("store lock").get(&digest).cloned();
        Box::pin(std::future::ready(Ok(png)))
    }
}
#[tokio::test]
async fn lazy_reuse_reads_only_matching_pngs() {
    let entries = covered();
    let first = drawn_unbounded(&entries).await;
    let store = Arc::new(MapStore::default());
    for letter in &first.letters {
        store
            .put(letter.png.clone())
            .await
            .expect("fixture PNG stores");
    }
    let mut known = stored_letters(&first);
    for letter in &mut known {
        letter.png = None;
    }
    let template = known.first().cloned().expect("fixture letter");
    for number in 0..100_u64 {
        let mut unrelated = template.clone();
        if let LetterRecord::Compaction { id, spans, .. } = &mut unrelated.record {
            *id = format!("history/9{number:03}.1");
            *spans = vec![Span {
                entry: entry(10_000 + number),
                part: 0,
                off: 0,
                len: 1,
            }];
        }
        known.push(unrelated);
    }
    let request =
        second_request(&entries, known).with_blob_store(Arc::clone(&store) as Arc<dyn BlobStore>);
    let (result, drawn) = run(limits(), profile(1000), request).await;
    assert!(result.expect("second run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    assert_eq!(
        store.gets.load(Ordering::SeqCst),
        first.letters.len(),
        "unrelated metadata never reaches blob_get"
    );
    assert_eq!(drawn.letters.len(), first.letters.len());
    assert!(drawn.letters.iter().all(|letter| letter.reused));
    let small = stored_letters(&first);
    let (_, small_drawn) = run(limits(), profile(1000), second_request(&entries, small)).await;
    let small_drawn = small_drawn.expect("small history draws");
    assert_eq!(
        drawn.letters, small_drawn.letters,
        "large history reuses the same images as small history"
    );
    assert_eq!(
        drawn.index[..small_drawn.index.len()],
        small_drawn.index,
        "large history keeps the small-history index lines first"
    );
    assert!(
        drawn.index.len() > small_drawn.index.len(),
        "unrelated compaction letters stay listed as not drawn"
    );
}
#[tokio::test]
async fn missing_reused_blob_redraws_the_same_source() {
    let entries = covered();
    let first = drawn_unbounded(&entries).await;
    let store = Arc::new(MapStore::default());
    for letter in &first.letters {
        store
            .put(letter.png.clone())
            .await
            .expect("fixture PNG stores");
    }
    let target = first.letters.first().expect("fixture letter");
    let target_id = target.record.id().to_owned();
    let target_spans = target.record.spans().to_vec();
    let LetterRecord::Compaction { png_blob, .. } = &target.record else {
        panic!("fixture compaction letter");
    };
    let digest = blake3::Hash::from_hex(png_blob).expect("fixture digest");
    store
        .blobs
        .lock()
        .expect("store lock")
        .remove(digest.as_bytes());
    let mut known = stored_letters(&first);
    for letter in &mut known {
        letter.png = None;
    }
    let request =
        second_request(&entries, known).with_blob_store(Arc::clone(&store) as Arc<dyn BlobStore>);
    let (result, drawn) = run(limits(), profile(1000), request).await;
    assert!(result.expect("second run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    let replacement = drawn
        .letters
        .iter()
        .find(|letter| letter.record.spans() == target_spans)
        .expect("missing image source is redrawn");
    assert_ne!(replacement.record.id(), target_id);
    assert!(!replacement.reused);
    assert_eq!(
        replacement.png, target.png,
        "the redrawn bytes match the exact source text"
    );
}

#[tokio::test]
async fn the_sink_stores_each_png_and_returns_image_parts() {
    let entries = covered();
    let request = request(&entries, budget(u64::MAX / 4, 0.7));
    let sink = Arc::new(PartsSink::new(
        Arc::new(MapStore::default()),
        "history".parse().expect("battery name"),
    ));
    let result = engine(limits())
        .run(request, profile(1000), sink as Arc<dyn Commit>)
        .await;
    let compaction = result.expect("the sink commits").expect("the compaction");
    let dal_agent::ext::Replacement::Parts {
        parts,
        letters,
        parts_tokens,
    } = compaction.replacement
    else {
        panic!("the replacement carries image parts");
    };
    let images = parts
        .iter()
        .filter(|part| matches!(part, Part::Image { .. }))
        .count();
    assert_ne!(letters.len(), 0);
    assert_eq!(images, letters.len());
    assert!(parts_tokens > 0);
    for record in &letters {
        assert_eq!(record.ext.as_str(), "history");
        assert_eq!(record.kind.as_ref(), "letter");
        let body = LetterRecord::decode(&record.body, entry(u64::MAX)).expect("the record decodes");
        assert!(matches!(body, LetterRecord::Compaction { .. }));
        assert!(body.id().starts_with("history/1."));
    }
}
fn unavailable<T: Send + 'static>() -> ServiceFuture<'static, T> {
    Box::pin(async {
        Err(ServiceError::failed(
            None,
            "unused in the history service-boundary test",
        ))
    })
}

#[derive(Default)]
struct CountingServices {
    records: Vec<RawJson>,
    blobs: Mutex<HashMap<[u8; 32], Vec<u8>>>,
    blob_gets: AtomicUsize,
}

impl CountingServices {
    fn locked_blobs(&self) -> std::sync::MutexGuard<'_, HashMap<[u8; 32], Vec<u8>>> {
        self.blobs.lock().expect("store lock")
    }
}

struct ServiceBlobs {
    services: Arc<dyn Services>,
    caller: Caller,
}

impl BlobStore for ServiceBlobs {
    fn put(&self, png: Vec<u8>) -> BoxFuture<'_, Result<[u8; 32], CompactError>> {
        let put = self.services.blob_put(&self.caller, png);
        Box::pin(async move { put.await.map_err(CompactError::from) })
    }

    fn get(&self, digest: [u8; 32]) -> BoxFuture<'_, Result<Option<Vec<u8>>, CompactError>> {
        let get = self.services.blob_get(&self.caller, digest);
        Box::pin(async move { get.await.map_err(CompactError::from) })
    }
}

impl Services for CountingServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }
    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unavailable()
    }
    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unavailable()
    }
    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unavailable()
    }
    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unavailable()
    }
    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unavailable()
    }
    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unavailable()
    }
    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        unavailable()
    }
    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        unavailable()
    }
    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        unavailable()
    }
    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        unavailable()
    }
    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        unavailable()
    }
    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unavailable()
    }
    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<Doc>> {
        unavailable()
    }
    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unavailable()
    }
    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }
    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unavailable()
    }
    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unavailable()
    }
    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        unavailable()
    }
    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unavailable()
    }
    fn notify(&self, _who: &Caller, _notice: Notice) {}
    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unavailable()
    }
    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        let records = self.records.iter().cloned().map(Box::new).collect();
        Box::pin(async move { Ok(records) })
    }
    fn blob_put(&self, _who: &Caller, bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        let digest = *blake3::hash(&bytes).as_bytes();
        self.locked_blobs().insert(digest, bytes);
        Box::pin(async move { Ok(digest) })
    }
    fn blob_get(&self, _who: &Caller, digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        self.blob_gets.fetch_add(1, Ordering::SeqCst);
        let png = self.locked_blobs().get(&digest).cloned();
        Box::pin(async move { Ok(png) })
    }
}

struct FixedHost {
    pieces: Vec<CompactPiece>,
    source: Arc<FixtureSource>,
    blobs: Arc<ServiceBlobs>,
}

impl ImageHost for FixedHost {
    fn source<'a>(
        &'a self,
        _input: &'a CompactInput<'_>,
        _services: &'a Arc<dyn Services>,
        _caller: &'a Caller,
    ) -> BoxFuture<'a, Result<(Vec<CompactPiece>, Arc<dyn SourceReader>), CompactError>> {
        let pieces = self.pieces.clone();
        let source = Arc::clone(&self.source) as Arc<dyn SourceReader>;
        Box::pin(async move { Ok((pieces, source)) })
    }

    fn blob_store(
        &self,
        _services: &Arc<dyn Services>,
        _caller: &Caller,
    ) -> Option<Arc<dyn BlobStore>> {
        Some(Arc::clone(&self.blobs) as Arc<dyn BlobStore>)
    }

    fn sink(&self, _services: &Arc<dyn Services>, caller: &Caller) -> Option<Arc<dyn Commit>> {
        Some(Arc::new(PartsSink::new(
            Arc::clone(&self.blobs) as Arc<dyn BlobStore>,
            caller.ext().clone(),
        )))
    }
}

fn boundary_records(first: &Drawn) -> Vec<RawJson> {
    let digest = "a".repeat(64);
    let mut records: Vec<RawJson> = Vec::with_capacity(103);
    for letter in &first.letters {
        let body = sonic_rs::to_string(&letter.record).expect("record encodes");
        records.push(RawJson::parse(&body).expect("record parses"));
    }
    for ordinal in 1000..1100 {
        records.push(
            RawJson::parse(&format!(
                r#"{{"v":1,"id":"history/{ordinal}.1","kind":"compaction","png_blob":"{digest}","png_bytes":1,"width":136,"height":16,"cell":[8,16],"spans":[[1,0,0,1]],"letters":[1]}}"#
            ))
            .expect("unrelated record parses"),
        );
    }
    records.push(
        RawJson::parse(
            r#"{"v":1,"id":"capture","kind":"skill","png_blob":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","png_bytes":1,"width":136,"height":16,"cell":[8,16],"spans":[]}"#,
        )
        .expect("skill record parses"),
    );
    records.push(
        RawJson::parse(
            r#"{"v":1,"id":"dream/1","kind":"dream","letters":["history/1.1"],"summary":"old"}"#,
        )
        .expect("dream record parses"),
    );
    records
}

fn assert_reused_compaction(compaction: &Compaction, services: &CountingServices, first: &Drawn) {
    assert_eq!(
        services.blob_gets.load(Ordering::SeqCst),
        first.letters.len(),
        "only matching PNGs cross the service boundary"
    );
    let dal_agent::ext::Replacement::Parts {
        parts,
        letters,
        parts_tokens,
    } = &compaction.replacement
    else {
        panic!("the replacement carries image parts");
    };
    assert!(*parts_tokens > 0);
    assert!(
        letters.is_empty(),
        "full reuse appends no fresh letter records"
    );
    let mut images: Vec<Vec<u8>> = parts
        .iter()
        .filter_map(|part| match part {
            Part::Image { bytes, .. } => Some(bytes.to_vec()),
            Part::Text { .. } | Part::Blob { .. } => None,
        })
        .collect();
    let mut expected: Vec<Vec<u8>> = first
        .letters
        .iter()
        .map(|letter| letter.png.clone())
        .collect();
    images.sort();
    expected.sort();
    assert_eq!(images, expected, "reused bytes match the stored PNGs");
    let text: String = parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.to_string()),
            Part::Image { .. } | Part::Blob { .. } => None,
        })
        .collect();
    assert!(
        !text.contains("dream/"),
        "dream summaries stay out of the image index"
    );
    assert!(
        text.contains("history/1000.1"),
        "unrelated compaction letters stay listed as not drawn"
    );
    assert!(
        !text.contains("letter://capture"),
        "skill captures stay out of the image index"
    );
}

#[tokio::test]
async fn compactor_reuses_matching_images_without_fetching_unrelated_pngs() {
    let entries = covered();
    let first = drawn_unbounded(&entries).await;
    assert!(
        first.letters.len() > 1,
        "the fixture draws reusable letters"
    );

    let records = boundary_records(&first);
    let services = Arc::new(CountingServices {
        records,
        blobs: Mutex::new(HashMap::new()),
        blob_gets: AtomicUsize::new(0),
    });
    for letter in &first.letters {
        services
            .locked_blobs()
            .insert(*blake3::hash(&letter.png).as_bytes(), letter.png.clone());
    }
    let (pieces, source) = pieces(&entries);
    let service_arc = Arc::clone(&services) as Arc<dyn Services>;
    let cx = ToolCx::for_test(Arc::clone(&service_arc));
    let host = FixedHost {
        pieces,
        source: Arc::new(source),
        blobs: Arc::new(ServiceBlobs {
            services: Arc::clone(&service_arc),
            caller: cx.caller().clone(),
        }),
    };
    let compactor = HistoryCompactor::with_host(0.7, Arc::new(host), limits());
    let input = CompactInput {
        caller: cx.caller(),
        model: ModelRoute::Api {
            family: Family::Chat,
            model: "test".into(),
        },
        session: SessionId::new_v7(),
        instructions: "",
        covered: &entries,
        span: (entry(1), entry(2)),
        first_kept: None,
        context_window: Some(u64::MAX / 4),
        image_profile: Some(profile(1000)),
        images_elsewhere: 0,
        image_bytes_elsewhere: 0,
        carried: None,
        total_tokens: 2_100_000,
        params: RequestParams::default(),
    };
    let compaction = compactor
        .compact(input, Arc::clone(&services) as Arc<dyn Services>)
        .await
        .expect("compaction runs")
        .expect("compaction emits");
    assert_reused_compaction(&compaction, &services, &first);
}

#[tokio::test]
async fn corrupt_reused_blob_redraws_the_same_source() {
    let entries = covered();
    let first = drawn_unbounded(&entries).await;
    let store = Arc::new(MapStore::default());
    for letter in &first.letters {
        store
            .put(letter.png.clone())
            .await
            .expect("fixture PNG stores");
    }
    let target = first.letters.first().expect("fixture letter");
    let target_id = target.record.id().to_owned();
    let target_spans = target.record.spans().to_vec();
    let LetterRecord::Compaction { png_blob, .. } = &target.record else {
        panic!("fixture compaction letter");
    };
    let digest = blake3::Hash::from_hex(png_blob).expect("fixture digest");
    store
        .blobs
        .lock()
        .expect("store lock")
        .insert(*digest.as_bytes(), vec![0_u8; 16]);
    let mut known = stored_letters(&first);
    for letter in &mut known {
        letter.png = None;
    }
    let request =
        second_request(&entries, known).with_blob_store(Arc::clone(&store) as Arc<dyn BlobStore>);
    let (result, drawn) = run(limits(), profile(1000), request).await;
    assert!(result.expect("second run succeeds").is_some());
    let drawn = drawn.expect("drawn");
    let replacement = drawn
        .letters
        .iter()
        .find(|letter| letter.record.spans() == target_spans)
        .expect("corrupt image source is redrawn");
    assert_ne!(replacement.record.id(), target_id);
    assert!(!replacement.reused);
    assert_eq!(
        replacement.png, target.png,
        "the redrawn bytes match the exact source text"
    );
}
