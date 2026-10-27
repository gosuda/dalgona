// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use super::common::{Host, Reply, TestResult, locked, run_tool};
use dal_agent::ext::{PromptOrder, PromptSection, Services, ToolCall, ToolCx, ToolOutcome};
use dal_core::Question;

const NO_ANSWER: &str = "No answer (dismissed, timed out, or no user attached). Continue on best judgment; do not ask again this turn.";

async fn ask_tool(
    host: &std::sync::Arc<Host>,
    args: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let extension = dalgona_batteries::ask::ask()?;
    run_tool(&extension, host, "ask", args).await
}

fn one(kind: &str, extra: &str) -> String {
    format!(
        r#"{{"questions":[{{"id":"pick","header":"Pick","question":"Which one?","kind":"{kind}"{extra}}}]}}"#
    )
}

const TWO_OPTIONS: &str =
    r#","options":[{"label":"Alpha"},{"label":"Beta","description":"second"}]"#;

#[tokio::test]
async fn single_answer_maps_to_a_header_line() -> TestResult {
    let controller = Host::answering([Reply::Value(r#"{"selected":["Alpha"]}"#)]);
    let text = ask_tool(&controller, &one("single", TWO_OPTIONS)).await?;
    assert_eq!(text, "Pick: Alpha");
    let asked = controller.asked();
    assert!(matches!(
        asked.as_slice(),
        [Question::Select { multi: false, options, .. }] if options.len() == 2
    ));
    Ok(())
}

#[tokio::test]
async fn multi_answer_joins_labels_and_caps_at_four() -> TestResult {
    let four = Host::answering([Reply::Value(r#"{"selected":["a","b","c","d"]}"#)]);
    assert_eq!(
        ask_tool(&four, &one("multi", TWO_OPTIONS)).await?,
        "Pick: a, b, c, d"
    );
    assert!(matches!(
        four.asked().as_slice(),
        [Question::Select { multi: true, .. }]
    ));
    for rejected in [
        r#"{"selected":["a","b","c","d","e"]}"#,
        r#"{"selected":[]}"#,
        r#"{"selected":["a",""]}"#,
        r#"{"selected":"a"}"#,
    ] {
        let controller = Host::answering([Reply::Value(rejected)]);
        assert_eq!(
            ask_tool(&controller, &one("multi", TWO_OPTIONS)).await?,
            NO_ANSWER,
            "{rejected}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn text_answer_is_trimmed_and_blank_text_is_no_answer() -> TestResult {
    let controller = Host::answering([Reply::Value(r#"{"text":"because"}"#)]);
    assert_eq!(
        ask_tool(&controller, &one("text", "")).await?,
        "Pick: because"
    );
    let padded = Host::answering([Reply::Value(r#"{"text":" use sqlite "}"#)]);
    assert_eq!(
        ask_tool(&padded, &one("text", "")).await?,
        "Pick: use sqlite"
    );
    assert!(matches!(
        controller.asked().as_slice(),
        [Question::Text { .. }]
    ));
    let blank = Host::answering([Reply::Value(r#"{"text":"   "}"#)]);
    assert_eq!(ask_tool(&blank, &one("text", "")).await?, NO_ANSWER);
    Ok(())
}

#[tokio::test]
async fn single_answer_must_be_a_non_empty_string() -> TestResult {
    for rejected in [
        r"{}",
        r#"{"selected":[]}"#,
        r#"{"selected":[""]}"#,
        r#"{"selected":["a","b"]}"#,
        r#"{"text":"a"}"#,
        "7",
        r#"["Alpha"]"#,
        r#""""#,
    ] {
        let controller = Host::answering([Reply::Value(rejected)]);
        assert_eq!(
            ask_tool(&controller, &one("single", TWO_OPTIONS)).await?,
            NO_ANSWER,
            "{rejected}"
        );
    }
    Ok(())
}

fn args_for(kind: &str) -> String {
    one(kind, if kind == "text" { "" } else { TWO_OPTIONS })
}

/// Both spellings an answering client may send decode to the same line: the
/// canonical bare value from the wire adapters and the strict object the
/// terminal dialog sends.
#[tokio::test]
async fn bare_and_object_spellings_answer_identically() -> TestResult {
    for (kind, payload, expected) in [
        ("single", r#""Alpha""#, "Pick: Alpha"),
        ("single", r#"{"selected":["Alpha"]}"#, "Pick: Alpha"),
        ("multi", r#"["a","b"]"#, "Pick: a, b"),
        ("multi", r#"{"selected":["a","b"]}"#, "Pick: a, b"),
        ("multi", r#"["a","b","c","d"]"#, "Pick: a, b, c, d"),
        ("multi", r#"["only"]"#, "Pick: only"),
        ("text", r#""because""#, "Pick: because"),
        ("text", r#"{"text":"because"}"#, "Pick: because"),
        ("text", r#"" use sqlite ""#, "Pick: use sqlite"),
        ("text", r#"{"text":" use sqlite "}"#, "Pick: use sqlite"),
    ] {
        let controller = Host::answering([Reply::Value(payload)]);
        assert_eq!(
            ask_tool(&controller, &args_for(kind)).await?,
            expected,
            "{kind} {payload}"
        );
        assert!(locked(&controller.notices).is_empty(), "{kind} {payload}");
        let asked = controller.asked();
        let raised_kind = match asked.as_slice() {
            [Question::Select { multi: false, .. }] => "single",
            [Question::Select { multi: true, .. }] => "multi",
            [Question::Text { .. }] => "text",
            other => return Err(format!("{payload}: unexpected questions {other:?}").into()),
        };
        assert_eq!(raised_kind, kind, "{payload}");
    }
    Ok(())
}

/// A payload of the wrong shape for its kind is malformed in either
/// spelling: no answer plus exactly one notice that names the question. An
/// object keeps its strict member set, so an unknown member never decodes
/// and never falls back to a bare-value reading.
#[tokio::test]
async fn wrong_shaped_answers_fail_closed_with_one_notice() -> TestResult {
    for (kind, payload) in [
        ("single", r#"["Alpha"]"#),
        ("single", "7"),
        ("single", "true"),
        ("single", "null"),
        ("single", r#"{"selected":["Alpha"],"role":"admin"}"#),
        ("single", r#"{"selected":"Alpha"}"#),
        ("single", r#"{"selected":["Alpha","Beta"]}"#),
        ("single", r#"{"selected":[""]}"#),
        ("single", r#"{"text":"Alpha"}"#),
        ("multi", r#""a""#),
        ("multi", r#"["a","b","c","d","e"]"#),
        ("multi", r#"["a",""]"#),
        ("multi", r#"["a",1]"#),
        ("multi", r#"[["a"]]"#),
        ("multi", r#"{"selected":["a"],"role":"admin"}"#),
        ("multi", r#"{"text":"a"}"#),
        ("text", r#"["a"]"#),
        ("text", "7"),
        ("text", "null"),
        ("text", r#"{"text":"a","role":"admin"}"#),
        ("text", r#"{"text":["a"]}"#),
        ("text", r#"{"selected":["a"]}"#),
    ] {
        let controller = Host::answering([Reply::Value(payload)]);
        assert_eq!(
            ask_tool(&controller, &args_for(kind)).await?,
            NO_ANSWER,
            "{kind} {payload}"
        );
        let notices = locked(&controller.notices);
        let [notice] = notices.as_slice() else {
            return Err(format!("{kind} {payload}: expected exactly one notice").into());
        };
        assert_eq!(&*notice.kind, "ask", "{kind} {payload}");
        assert!(
            notice
                .text
                .contains("malformed answer for question \"Pick\""),
            "{kind} {payload}: {}",
            notice.text
        );
    }
    Ok(())
}

/// A blank submission dismisses the question in either spelling: no answer
/// and no notice, unlike a malformed payload.
#[tokio::test]
async fn blank_answers_dismiss_without_a_notice() -> TestResult {
    for (kind, payload) in [
        ("single", r#""""#),
        ("single", r#""   ""#),
        ("single", r#"{"selected":[]}"#),
        ("multi", "[]"),
        ("multi", r#"{"selected":[]}"#),
        ("text", r#""""#),
        ("text", r#""  ""#),
        ("text", r#"{"text":"  "}"#),
    ] {
        let controller = Host::answering([Reply::Value(payload)]);
        assert_eq!(
            ask_tool(&controller, &args_for(kind)).await?,
            NO_ANSWER,
            "{kind} {payload}"
        );
        assert!(locked(&controller.notices).is_empty(), "{kind} {payload}");
    }
    Ok(())
}

#[tokio::test]
async fn one_call_takes_both_spellings_and_stops_at_a_malformed_answer() -> TestResult {
    let mixed = Host::answering([
        Reply::Value(r#""yes""#),
        Reply::Value(r#"{"text":"b"}"#),
        Reply::Value(r#""c""#),
    ]);
    assert_eq!(
        ask_tool(&mixed, &three_questions()).await?,
        "One: yes\nTwo: b\nThree: c"
    );
    assert!(locked(&mixed.notices).is_empty());

    let stopped = Host::answering([
        Reply::Value(r#""yes""#),
        Reply::Value(r#"{"text":"b","role":"admin"}"#),
    ]);
    assert_eq!(
        ask_tool(&stopped, &three_questions()).await?,
        "One: yes\nUnanswered: Two, Three"
    );
    assert_eq!(stopped.asked().len(), 2, "the third is never raised");
    let notices = locked(&stopped.notices);
    let [notice] = notices.as_slice() else {
        return Err("expected exactly one notice".into());
    };
    assert!(
        notice
            .text
            .contains("malformed answer for question \"Two\""),
        "{}",
        notice.text
    );
    Ok(())
}

#[tokio::test]
async fn preview_reaches_the_select_question_under_the_header_title() -> TestResult {
    let controller = Host::answering([Reply::Value(r#"{"selected":["Alpha"]}"#)]);
    let extra = format!(r#"{TWO_OPTIONS},"preview":"--- a\n+++ b""#);
    ask_tool(&controller, &one("single", &extra)).await?;
    let asked = controller.asked();
    let [
        Question::Select {
            preview: Some(preview),
            ..
        },
    ] = asked.as_slice()
    else {
        return Err("the select question carries no preview".into());
    };
    assert_eq!(&*preview.title, "Pick");
    assert_eq!(&*preview.body, "--- a\n+++ b");
    Ok(())
}

#[tokio::test]
async fn dismissal_and_failure_fail_closed_with_the_literal() -> TestResult {
    for reply in [Reply::Dismissed, Reply::Failed, Reply::Cancelled] {
        let controller = Host::answering([reply]);
        assert_eq!(
            ask_tool(&controller, &one("single", TWO_OPTIONS)).await?,
            NO_ANSWER
        );
    }
    let silent = Host::answering([]);
    assert_eq!(ask_tool(&silent, &one("text", "")).await?, NO_ANSWER);
    assert_eq!(silent.asked().len(), 1);
    Ok(())
}

fn three_questions() -> String {
    let question = |id: &str, header: &str| {
        format!(r#"{{"id":"{id}","header":"{header}","question":"Which?","kind":"text"}}"#)
    };
    format!(
        r#"{{"questions":[{},{},{}]}}"#,
        question("first", "One"),
        question("second", "Two"),
        question("third", "Three")
    )
}

#[tokio::test]
async fn a_dismissal_after_answers_lists_the_unanswered_headers() -> TestResult {
    let controller = Host::answering([Reply::Value(r#"{"text":"yes"}"#), Reply::Dismissed]);
    let text = ask_tool(&controller, &three_questions()).await?;
    assert_eq!(text, "One: yes\nUnanswered: Two, Three");
    assert_eq!(controller.asked().len(), 2);
    Ok(())
}

#[tokio::test]
async fn every_question_answered_yields_one_line_each() -> TestResult {
    let controller = Host::answering([
        Reply::Value(r#"{"text":"a"}"#),
        Reply::Value(r#"{"text":"b"}"#),
        Reply::Value(r#"{"text":"c"}"#),
    ]);
    assert_eq!(
        ask_tool(&controller, &three_questions()).await?,
        "One: a\nTwo: b\nThree: c"
    );
    Ok(())
}

fn invalid_question_cases() -> Vec<(String, &'static str)> {
    let long_header = "h".repeat(13);
    vec![
        (
            r#"{"questions":[]}"#.to_owned(),
            "ask: questions must contain 1 to 4 items",
        ),
        (
            one("single", TWO_OPTIONS).replace("\"pick\"", "\"Pick\""),
            "ask: id must be snake_case (a-z, 0-9, underscores, starting with a letter)",
        ),
        (
            one("single", TWO_OPTIONS).replace("\"pick\"", "\"bad__id\""),
            "ask: id must be snake_case (a-z, 0-9, underscores, starting with a letter)",
        ),
        (
            one("single", TWO_OPTIONS).replace("\"Pick\"", &format!("\"{long_header}\"")),
            "ask: header must be 1 to 12 characters",
        ),
        (
            one("single", TWO_OPTIONS).replace("Which one?", "  "),
            "ask: question must be non-empty",
        ),
        (
            one("poll", TWO_OPTIONS),
            "ask: kind must be one of single, multi, text",
        ),
        (
            one("text", TWO_OPTIONS),
            "ask: a text question takes no options",
        ),
        (
            one("single", ""),
            "ask: a single or multi question needs options",
        ),
        (
            one("single", r#","options":[{"label":"Only"}]"#),
            "ask: options must contain 2 to 4 items",
        ),
        (
            one(
                "single",
                r#","options":[{"label":"a"},{"label":"b"},{"label":"c"},{"label":"d"},{"label":"e"}]"#,
            ),
            "ask: options must contain 2 to 4 items",
        ),
    ]
}

fn invalid_value_cases() -> Vec<(String, &'static str)> {
    let control_label = "bad\\u0007label";
    vec![
        (
            one("single", r#","options":[{"label":"  "},{"label":"b"}]"#),
            "ask: option label must be non-empty",
        ),
        (
            one(
                "single",
                &format!(r#","options":[{{"label":"{control_label}"}},{{"label":"b"}}]"#),
            ),
            "ask: option label must not contain control characters",
        ),
        (
            one(
                "single",
                r#","options":[{"label":"a","description":" "},{"label":"b"}]"#,
            ),
            "ask: option description must be non-empty when present",
        ),
        (
            one(
                "single",
                &format!(
                    r#","options":[{{"label":"a","description":"{control_label}"}},{{"label":"b"}}]"#
                ),
            ),
            "ask: option description must not contain control characters",
        ),
        (
            one("text", r#","preview":"body""#),
            "ask: preview is allowed only on single and multi questions",
        ),
        (
            one("single", &format!(r#"{TWO_OPTIONS},"preview":"""#)),
            "ask: preview must be 1 to 8192 bytes",
        ),
        (
            one(
                "single",
                &format!(r#"{TWO_OPTIONS},"preview":"{}""#, "p".repeat(8193)),
            ),
            "ask: preview must be 1 to 8192 bytes",
        ),
        (
            r#"{"questions":[{"id":"x","header":"X","question":"q","kind":"text"},{"id":"x","header":"Y","question":"q","kind":"text"}]}"#.to_owned(),
            "ask: question ids must be unique; \"x\" repeats",
        ),
        (
            r#"{"questions":[{"id":"x","header":"X","question":"q","kind":"text"},{"id":"y","header":"X","question":"q","kind":"text"}]}"#.to_owned(),
            "ask: header must be unique within one call",
        ),
        (
            r#"{"questions":true}"#.to_owned(),
            "ask: invalid type: boolean `true`, expected a sequence at line 1 column 17",
        ),
        (
            r#"{"nope":1}"#.to_owned(),
            "ask: unknown field `nope`, expected `questions` or `explicit` at line 1 column 8",
        ),
    ]
}

#[tokio::test]
async fn invalid_input_is_rejected_before_any_question_is_raised() -> TestResult {
    for (args, expected) in invalid_question_cases()
        .into_iter()
        .chain(invalid_value_cases())
    {
        let controller = Host::answering([Reply::Value(r#"{"selected":["Alpha"]}"#)]);
        assert_eq!(ask_tool(&controller, &args).await?, expected, "{args}");
        assert!(controller.asked().is_empty(), "{args}");
    }
    Ok(())
}

#[tokio::test]
async fn five_questions_are_over_the_cap() -> TestResult {
    let question = |index: usize| {
        format!(r#"{{"id":"q{index}","header":"H{index}","question":"Which?","kind":"text"}}"#)
    };
    let all: Vec<String> = (0..5).map(question).collect();
    let args = format!(r#"{{"questions":[{}]}}"#, all.join(","));
    let controller = Host::answering([]);
    assert_eq!(
        ask_tool(&controller, &args).await?,
        "ask: questions must contain 1 to 4 items"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// g08 remaining rows: order pin, boundaries, notices, properties, timing.

#[tokio::test]
async fn kind_is_validated_before_the_id() -> TestResult {
    // Same question carries a bad kind and a bad id: the kind error wins.
    let args = one("poll", "").replace("\"pick\"", "\"Pick\"");
    let controller = Host::answering([]);
    assert_eq!(
        ask_tool(&controller, &args).await?,
        "ask: kind must be one of single, multi, text",
    );
    assert_eq!(controller.asked().len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_twelve_character_cjk_header_passes_and_a_thirteenth_fails() -> TestResult {
    let twelve = "\u{d55c}\u{ae00}".repeat(6);
    let args = one("text", "").replace("\"Pick\"", &format!("\"{twelve}\""));
    let controller = Host::answering([Reply::Value(r#"{"text":"a"}"#)]);
    ask_tool(&controller, &args).await?;
    assert_eq!(controller.asked().len(), 1, "12 CJK characters fit");

    let thirteen = format!("{twelve}\u{b9c8}");
    let args = one("text", "").replace("\"Pick\"", &format!("\"{thirteen}\""));
    let controller = Host::answering([]);
    assert_eq!(
        ask_tool(&controller, &args).await?,
        "ask: header must be 1 to 12 characters",
    );
    assert!(controller.asked().is_empty(), "{thirteen}");
    Ok(())
}

#[tokio::test]
async fn an_exactly_8192_byte_preview_passes() -> TestResult {
    let extra = format!(r#"{TWO_OPTIONS},"preview":"{}""#, "p".repeat(8192));
    let controller = Host::answering([Reply::Value(r#"{"selected":["Alpha"]}"#)]);
    ask_tool(&controller, &one("single", &extra)).await?;
    let asked = controller.asked();
    let [
        Question::Select {
            preview: Some(_), ..
        },
    ] = asked.as_slice()
    else {
        return Err("the 8192-byte preview must reach the raise".into());
    };

    // One byte over the cap fails closed with the typed length error before
    // any question is raised, so the cap is proven from both sides.
    let over = format!(r#"{TWO_OPTIONS},"preview":"{}""#, "p".repeat(8193));
    let refused = Host::answering([]);
    assert_eq!(
        ask_tool(&refused, &one("single", &over)).await?,
        "ask: preview must be 1 to 8192 bytes",
    );
    assert!(
        refused.asked().is_empty(),
        "the 8193-byte preview must not raise"
    );
    Ok(())
}

#[tokio::test]
async fn two_silent_questions_stay_all_unanswered_after_one_raise() -> TestResult {
    let args = format!(
        r#"{{"questions":[{},{}]}}"#,
        r#"{"id":"first","header":"One","question":"Which?","kind":"text"}"#,
        r#"{"id":"second","header":"Two","question":"Which?","kind":"text"}"#,
    );
    let controller = Host::answering([]);
    assert_eq!(ask_tool(&controller, &args).await?, NO_ANSWER);
    assert_eq!(controller.asked().len(), 1, "no second raise");
    Ok(())
}

#[tokio::test]
async fn a_malformed_answer_is_no_answer_with_one_named_notice() -> TestResult {
    for payload in [
        r"{}",
        r#"{"selected":["a","b"]}"#,
        r#"{"text":"untrimmed  "}"#,
    ] {
        let controller = Host::answering([Reply::Value(payload)]);
        assert_eq!(
            ask_tool(&controller, &one("single", TWO_OPTIONS)).await?,
            NO_ANSWER,
            "{payload}"
        );
        let notices = locked(&controller.notices);
        let [notice] = notices.as_slice() else {
            return Err(format!("{payload}: expected exactly one notice").into());
        };
        assert!(
            notice
                .text
                .contains("malformed answer for question \"Pick\""),
            "{payload}: {}",
            notice.text
        );
    }
    Ok(())
}

#[tokio::test]
async fn explicit_flag_is_accepted_and_ignored_by_the_battery() -> TestResult {
    let args = format!(
        r#"{{"explicit":true,"questions":[{}]}}"#,
        r#"{"id":"pick","header":"Pick","question":"Which one?","kind":"text"}"#
    );
    let controller = Host::answering([Reply::Value(r#"{"text":"a"}"#)]);
    assert_eq!(ask_tool(&controller, &args).await?, "Pick: a");
    Ok(())
}

#[tokio::test]
async fn two_answers_then_a_dismissal_list_the_last_two_headers() -> TestResult {
    let question = |id: &str, header: &str| {
        format!(r#"{{"id":"{id}","header":"{header}","question":"Which?","kind":"text"}}"#)
    };
    let args = format!(
        r#"{{"questions":[{},{},{},{}]}}"#,
        question("q1", "One"),
        question("q2", "Two"),
        question("q3", "Three"),
        question("q4", "Four"),
    );
    let controller = Host::answering([
        Reply::Value(r#"{"text":"a"}"#),
        Reply::Value(r#"{"text":"b"}"#),
        Reply::Dismissed,
    ]);
    assert_eq!(
        ask_tool(&controller, &args).await?,
        "One: a\nTwo: b\nUnanswered: Three, Four",
    );
    assert_eq!(controller.asked().len(), 3, "the fourth is never raised");
    Ok(())
}

/// An immediate `None` is the merged no-answer for a dismissal, a timeout, or
/// a missing controller. The tool must settle it on its first poll, so it never
/// parks on an approval deadline, an answer channel, or a timer. One poll with
/// a no-op waker makes that a property of the control flow: elapsed time under
/// a loaded runner proves nothing either way.
#[tokio::test]
async fn an_immediate_none_returns_without_waiting_for_an_answer() -> TestResult {
    let controller = Host::answering([]);
    let args = one("text", "");
    let mut tool = std::pin::pin!(ask_tool(&controller, &args));
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    let std::task::Poll::Ready(text) = Future::poll(tool.as_mut(), &mut context) else {
        return Err("an immediate None must settle on its first poll".into());
    };
    assert_eq!(text?, NO_ANSWER);
    assert_eq!(controller.asked().len(), 1, "the question is raised once");
    assert!(
        locked(&controller.notices).is_empty(),
        "an immediate None carries no malformed-answer notice"
    );
    Ok(())
}

/// The independent composer oracle, written from the documented templates only.
mod composer {
    /// One outcome of the per-call loop.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Outcome {
        Answered { header: String, answer: String },
        Unanswered { headers: Vec<String> },
        AllUnanswered,
    }

    /// The independent parser: recovers answered lines, their answers, and
    /// the unanswered set from composed text.
    pub(super) fn parse(text: &str, headers: &[&str]) -> Vec<Outcome> {
        if text == super::NO_ANSWER {
            return vec![Outcome::AllUnanswered];
        }
        let mut outcomes = Vec::new();
        let mut lines = text.lines().peekable();
        while let Some(line) = lines.peek() {
            let Some(header) = headers
                .iter()
                .find(|header| line.starts_with(&format!("{header}: ")))
            else {
                break;
            };
            let mut answer = line[header.len() + 2..].to_owned();
            lines.next();
            // A text answer may embed newlines: continuation lines belong to
            // the same answer until the next known header prefix.
            while let Some(rest) = lines.peek() {
                if headers
                    .iter()
                    .any(|header| rest.starts_with(&format!("{header}: ")))
                    || rest.starts_with("Unanswered: ")
                {
                    break;
                }
                answer.push('\n');
                answer.push_str(rest);
                lines.next();
            }
            outcomes.push(Outcome::Answered {
                header: (*header).to_owned(),
                answer,
            });
        }
        if let Some(rest) = lines.next() {
            let headers: Vec<String> = rest
                .strip_prefix("Unanswered: ")
                .unwrap_or(rest)
                .split(", ")
                .map(str::to_owned)
                .collect();
            outcomes.push(Outcome::Unanswered { headers });
        }
        if outcomes.is_empty() {
            outcomes.push(Outcome::AllUnanswered);
        }
        outcomes
    }
}

#[tokio::test]
async fn the_composed_result_parses_back_to_exactly_what_the_controller_answered() -> TestResult {
    use composer::Outcome;

    let headers = ["One", "Two", "Three"];
    let answered = |header: &str, answer: &str| Outcome::Answered {
        header: header.to_owned(),
        answer: answer.to_owned(),
    };

    let controller = Host::answering([
        Reply::Value(r#""a""#),
        Reply::Value(r#"{"text":"line one\nline two"}"#),
        Reply::Dismissed,
    ]);
    let text = ask_tool(&controller, &three_questions()).await?;
    assert_eq!(
        composer::parse(&text, &headers),
        vec![
            answered("One", "a"),
            answered("Two", "line one\nline two"),
            Outcome::Unanswered {
                headers: vec!["Three".to_owned()],
            },
        ],
        "{text}"
    );

    let all = Host::answering([
        Reply::Value(r#""a""#),
        Reply::Value(r#""b""#),
        Reply::Value(r#""c""#),
    ]);
    let text = ask_tool(&all, &three_questions()).await?;
    assert_eq!(
        composer::parse(&text, &headers),
        vec![
            answered("One", "a"),
            answered("Two", "b"),
            answered("Three", "c"),
        ],
        "{text}"
    );

    let none = Host::answering([Reply::Dismissed]);
    let text = ask_tool(&none, &three_questions()).await?;
    assert_eq!(
        composer::parse(&text, &headers),
        vec![Outcome::AllUnanswered]
    );
    Ok(())
}

/// The independent validator oracle: for every generated document it must
/// accept exactly what the battery accepts and cite a real 4.4 reason.
mod oracle {
    use super::build_question;

    /// One generated question variant: (label, id, header, kind, options,
    /// preview) with the expected verdict.
    pub(super) fn cases() -> Vec<(String, Option<&'static str>)> {
        let mut cases = Vec::new();
        for kind in ["single", "multi", "text"] {
            for id in ["pick", "Pick", "bad__id", "trailing_"] {
                for header in ["Pick", "", &"h".repeat(13)] {
                    for options in ["", super::TWO_OPTIONS, r#","options":[{"label":"a"}]"#] {
                        for preview in ["", r#","preview":"d""#] {
                            let question = build_question(id, header, kind, options, preview);
                            cases.push((
                                question,
                                expected_verdict(kind, id, header, options, preview),
                            ));
                        }
                    }
                }
            }
        }
        cases
    }

    fn expected_verdict(
        kind: &str,
        id: &str,
        header: &str,
        options: &str,
        preview: &str,
    ) -> Option<&'static str> {
        let select = kind != "text";
        let has_options = !options.is_empty();
        let has_preview = !preview.is_empty();
        let id_ok = id
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            && !id.contains("__")
            && !id.ends_with('_');
        let header_ok = !header.is_empty() && header.chars().count() <= 12;
        if select != has_options {
            return None;
        }
        if select && options.len() > 2 && options.split("{\"label\"").count() - 1 < 2 {
            return None;
        }
        if has_preview && !select {
            return None;
        }
        let id_and_header_ok = id_ok && header_ok;
        id_and_header_ok.then_some("accepted")
    }
}

/// Builds one question object the way the table-driven generator needs.
fn build_question(id: &str, header: &str, kind: &str, options: &str, preview: &str) -> String {
    format!(
        r#"{{"id":"{id}","header":"{header}","question":"Which?","kind":"{kind}"{options}{preview}}}"#
    )
}

#[tokio::test]
async fn the_battery_decides_exactly_what_the_oracle_decides() -> TestResult {
    for (question, expected) in oracle::cases() {
        let args = format!(r#"{{"questions":[{question}]}}"#);
        let controller = Host::answering([]);
        let outcome = ask_tool(&controller, &args).await?;
        match expected {
            None => {
                assert!(
                    outcome.starts_with("ask: "),
                    "rejections cite a reason: {outcome} for {question}"
                );
                assert!(controller.asked().is_empty(), "{question}");
            }
            Some(verdict) => {
                assert_eq!(verdict, "accepted");
                assert_eq!(outcome, NO_ANSWER, "{question}");
                assert_eq!(controller.asked().len(), 1, "{question}");
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Registration literals, the cancel-token arm, drifted inputs, and doc
// invariants.

const EXPECTED_DESCRIPTION: &str = "Ask the user one to four short questions and wait for the answers. Each question is\nsingle-select, multi-select, or free text; add a preview only when seeing a plan or a\ndiff would change the choice. Apply two filters before asking: if the evidence already\ncollected can answer the question, explore instead; if the ideal state settles it,\nresolve to that instead. An owner decision always survives as a question: irreversible,\ndestructive, safety-critical, or cross-cutting product choices. If the call returns no\nanswer, continue on best judgment and do not ask again this turn. Never use it for\npermission requests; permission goes through approval.";

const EXPECTED_SECTION: &str = "When you consider asking the user a question, apply two filters. First: could the\nevidence already collected answer it? Then explore instead of asking. Second: does the\nideal state settle it? Then resolve to that state instead of asking. An owner decision\nalways survives as a question: irreversible or destructive actions, safety-critical\nchoices, and cross-cutting product choices. Explore to sufficiency, then stop.";

const EXPECTED_PARAMETERS: &str = r#"{"type":"object","additionalProperties":false,"required":["questions"],"properties":{"explicit":{"type":"boolean","default":false,"description":"true when the user asked for this question directly"},"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","additionalProperties":false,"required":["id","header","question","kind"],"properties":{"id":{"type":"string","pattern":"^[a-z][a-z0-9]*(_[a-z0-9]+)*$"},"header":{"type":"string","minLength":1,"maxLength":12},"question":{"type":"string","minLength":1},"kind":{"type":"string","enum":["single","multi","text"]},"options":{"type":"array","minItems":2,"maxItems":4,"items":{"type":"object","additionalProperties":false,"required":["label"],"properties":{"label":{"type":"string","minLength":1},"description":{"type":"string","minLength":1}}}},"preview":{"type":"string","minLength":1,"maxLength":8192}}}}}}"#;

fn test_model() -> dal_core::ModelInfo {
    dal_core::ModelInfo {
        route: dal_core::ModelRoute::Synthetic {
            id: "test/model".into(),
        },
        name: "test".into(),
        provider: "".into(),
        caps: dal_core::Caps {
            context_window: None,
            thinking: Box::<[dal_core::ThinkingLevel]>::default(),
            tool_use: true,
            image_input: false,
            custom_grammar: false,
        },
    }
}

/// Spec test `registration_literals`: the extension
/// record pins the tool name, the description bytes, the parameter schema,
/// the single prompt section, `inject = ["ask"]`, and origin `Bundled`.
/// Every literal is typed again here, never read from the battery.
#[tokio::test]
async fn registration_pins_the_tool_record_schema_and_policy_strings() -> TestResult {
    let extension = dalgona_batteries::ask::ask()?;
    assert_eq!(extension.name(), "ask");
    let inject = extension.inject();
    assert_eq!(inject.iter().count(), 1);
    assert!(inject.contains(dal_core::Service::Ask));
    assert_eq!(extension.origin(), dal_core::Origin::Bundled);

    let [(tool, visibility)] = extension.tools() else {
        return Err("the ask extension publishes exactly one tool".into());
    };
    assert_eq!(tool.name().as_str(), "ask");
    assert_eq!(*visibility, dal_core::Visibility::Model);
    let spec = tool.spec(&test_model());
    assert_eq!(&*spec.description, EXPECTED_DESCRIPTION);
    let parameters: sonic_rs::Value = sonic_rs::from_str(spec.parameters.as_str())
        .map_err(|error| format!("registered parameters parse: {error}"))?;
    let expected: sonic_rs::Value = sonic_rs::from_str(EXPECTED_PARAMETERS)
        .map_err(|error| format!("expected schema parses: {error}"))?;
    assert_eq!(parameters, expected);

    let [section] = extension.prompt_sections() else {
        return Err("the ask extension carries exactly one prompt section".into());
    };
    let PromptSection::Static {
        order,
        visibility,
        text,
    } = section
    else {
        return Err("the ask prompt section is static text".into());
    };
    assert_eq!(*order, PromptOrder::Plugins);
    assert_eq!(*visibility, dal_core::Visibility::Model);
    assert_eq!(&**text, EXPECTED_SECTION);
    Ok(())
}

/// Battery half of spec test `turn-cancel-mid-questionnaire`: a
/// `Cancelled` reply with the turn token cancelled is the documented
/// `tool.interrupted` outcome; the same reply with no cancel requested fails
/// closed to the literal. The broker broadcast half is loop-owned.
#[tokio::test]
async fn a_cancelled_turn_maps_a_cancelled_reply_to_interrupted() -> TestResult {
    let extension = dalgona_batteries::ask::ask()?;
    let Some((tool, _)) = extension
        .tools()
        .iter()
        .find(|(tool, _)| tool.name().as_str() == "ask")
    else {
        return Err("the ask tool is not registered".into());
    };
    let tool = std::sync::Arc::clone(tool);
    let args = one("single", TWO_OPTIONS);

    let cancelled = Host::answering([Reply::Cancelled]);
    let services: std::sync::Arc<dyn Services> = cancelled.clone();
    let cx = ToolCx::for_test(services);
    cx.cancel().cancel();
    let call = ToolCall::new("call", dal_core::RawJson::parse(&args)?);
    match tool.run(call, cx).await {
        ToolOutcome::Interrupted => {}
        ToolOutcome::Ok(output) => {
            return Err(format!("a cancelled turn answered: {output}").into());
        }
        ToolOutcome::Err(error) => {
            return Err(format!("a cancelled turn errored: {error}").into());
        }
        ToolOutcome::Detached(_) => return Err("a cancelled turn detached".into()),
    }
    assert_eq!(cancelled.asked().len(), 1);

    let plain = Host::answering([Reply::Cancelled]);
    assert_eq!(ask_tool(&plain, &args).await?, NO_ANSWER);
    assert!(locked(&plain.notices).is_empty());
    Ok(())
}

/// Inputs the contract never issued fail closed with the documented
/// `ask: <reason>` error before any question is raised: a
/// Starlark-era spelling, a non-object root, and the two `snake_case` edges
/// the table never named. An empty body is not valid JSON and never becomes
/// a tool call, so the battery cannot see it; it is excluded here.
#[tokio::test]
async fn drifted_and_non_object_inputs_fail_closed_before_any_raise() -> TestResult {
    let snake_case = "ask: id must be snake_case (a-z, 0-9, underscores, starting with a letter)";
    let waited = one("single", TWO_OPTIONS).replace(
        "\"kind\":\"single\"",
        "\"kind\":\"single\",\"wait_for_answer\":true",
    );
    let trailing = one("single", TWO_OPTIONS).replace("\"pick\"", "\"pick_\"");
    let empty_id = one("single", TWO_OPTIONS).replace("\"pick\"", "\"\"");
    let cases: Vec<(String, &str)> = vec![
        (waited, "ask: unknown field `wait_for_answer`"),
        (trailing, snake_case),
        (empty_id, snake_case),
        (String::from("[]"), "ask: "),
        (String::from(r#"{"questions":null}"#), "ask: "),
    ];
    for (args, prefix) in cases {
        let controller = Host::answering([Reply::Value(r#"{"selected":["Alpha"]}"#)]);
        let outcome = ask_tool(&controller, &args).await?;
        assert!(
            outcome.starts_with(prefix),
            "{args}: {outcome} names {prefix}"
        );
        assert!(controller.asked().is_empty(), "{args}");
    }
    Ok(())
}

/// The `dalgona://ask` page documents the timeout, clamp, answer-object,
/// and never-answerer contract it owns in this surface. The
/// heading stays the reconciled Rust-carrier text, not the Starlark page.
#[tokio::test]
async fn ask_doc_documents_the_timeout_and_no_answer_contract() -> TestResult {
    let doc = dalgona_batteries::ask::ASK_DOC;
    for phrase in [
        "[ask] timeout",
        "30 minutes",
        "1 to 120 minutes",
        "{\"selected\"",
        "{\"text\"",
        "Print, JSON, and child sessions are never answerers",
        "continue on best judgment and do not ask again",
    ] {
        assert!(doc.contains(phrase), "ASK_DOC documents {phrase}");
    }
    Ok(())
}
