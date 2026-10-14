// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use super::common::{Host, Reply, TestResult, locked, run_tool};
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
        r#""Alpha""#,
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

#[tokio::test]
async fn an_immediate_none_answers_in_under_fifty_milliseconds() -> TestResult {
    let started = std::time::Instant::now();
    let controller = Host::answering([]);
    assert_eq!(ask_tool(&controller, &one("text", "")).await?, NO_ANSWER);
    assert!(
        started.elapsed() < std::time::Duration::from_millis(50),
        "{:?}",
        started.elapsed()
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
async fn the_composer_round_trips_through_an_independent_parser() -> TestResult {
    use composer::Outcome;

    let cases: Vec<(String, Vec<&str>, Vec<Outcome>)> = vec![
        (
            "Pick: Alpha".to_owned(),
            vec!["Pick"],
            vec![Outcome::Answered {
                header: "Pick".to_owned(),
                answer: "Alpha".to_owned(),
            }],
        ),
        (
            "One: a\nUnanswered: Two, Three".to_owned(),
            vec!["One", "Two", "Three"],
            vec![
                Outcome::Answered {
                    header: "One".to_owned(),
                    answer: "a".to_owned(),
                },
                Outcome::Unanswered {
                    headers: vec!["Two".to_owned(), "Three".to_owned()],
                },
            ],
        ),
        (
            NO_ANSWER.to_owned(),
            vec!["One"],
            vec![Outcome::AllUnanswered],
        ),
        (
            "One: line one\nline two\nTwo: b".to_owned(),
            vec!["One", "Two"],
            vec![
                Outcome::Answered {
                    header: "One".to_owned(),
                    answer: "line one\nline two".to_owned(),
                },
                Outcome::Answered {
                    header: "Two".to_owned(),
                    answer: "b".to_owned(),
                },
            ],
        ),
    ];
    for (text, headers, expected) in cases {
        assert_eq!(composer::parse(&text, &headers), expected, "{text}");
    }
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
