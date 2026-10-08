//! Verifies queued TUI requests render and answer exactly once.

use std::time::{Duration, Instant};

use dal_core::{Answer, Owner, Preview, Question, Request, RequestId};
use dal_tui::{
    WidthMode,
    dialog::DialogUi,
    keys::{InputEvent, KeyDecoder},
};

#[test]
fn tui_shows_two_requests_and_resolves_each_once() {
    let first = approval_request();
    let second = approval_request();
    let first_id = first.id;
    let second_id = second.id;
    let mut dialog = DialogUi::default();
    dialog.opened(first);
    dialog.opened(second);

    let (shown, waiting) = dialog.queue.shown().unwrap();
    assert_eq!(shown.id, first_id);
    assert_eq!(waiting, 1);
    let rows = dialog.rows(80, 24, WidthMode::Narrow).join("\n");
    assert!(rows.contains(dal_tui::copy::ids::APPROVAL_TITLE_COMMAND));
    assert!(rows.contains("1 more waiting"));

    let answer_key = key(b"y").expect("ordinary key bytes decode as a key");
    let (answered, answer) = dialog
        .key(answer_key)
        .expect("the first request accepts an answer");
    assert_eq!(answered, first_id);
    assert_eq!(answer, Answer::Approve);
    assert!(
        dialog.key(answer_key).is_none(),
        "the same request cannot be answered twice"
    );

    let first_text = first_id.to_string();
    dialog
        .resolved(&first_text)
        .expect("the first open request resolves");
    let (shown, waiting) = dialog
        .queue
        .shown()
        .expect("the queued request advances to the dialog");
    assert_eq!(shown.id, second_id);
    assert_eq!(waiting, 0);

    let (answered, answer) = dialog
        .key(answer_key)
        .expect("the second request accepts an answer");
    assert_eq!(answered, second_id);
    assert_eq!(answer, Answer::Approve);
    assert!(
        dialog.key(answer_key).is_none(),
        "the second request also answers only once"
    );
    let second_text = second_id.to_string();
    dialog
        .resolved(&second_text)
        .expect("the second open request resolves");
    assert!(!dialog.is_open());
}

fn approval_request() -> Request {
    Request {
        id: RequestId::new_v7(),
        turn: None,
        owner: Owner::Core,
        question: Question::Approval {
            tool: "exec".into(),
            preview: Preview {
                title: "Run a command".into(),
                body: "sleep 1".into(),
                digest: None,
            },
            grant: None,
            call: None,
        },
        timeout: Duration::from_secs(30),
        default: Answer::Decline,
    }
}

fn key(bytes: &[u8]) -> Option<dal_tui::keys::Key> {
    let mut decoder = KeyDecoder::default();
    let event = decoder.feed(bytes, Instant::now()).into_iter().next()?;
    match event {
        InputEvent::Key(key) => Some(key),
        InputEvent::Paste(_) => None,
    }
}
