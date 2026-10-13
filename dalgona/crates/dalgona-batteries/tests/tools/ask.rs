// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use super::common::{Host, Reply, TestResult, run_tool};
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
    let controller = Host::answering([Reply::Value(r#""Alpha""#)]);
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
    let four = Host::answering([Reply::Value(r#"["a","b","c","d"]"#)]);
    assert_eq!(
        ask_tool(&four, &one("multi", TWO_OPTIONS)).await?,
        "Pick: a, b, c, d"
    );
    assert!(matches!(
        four.asked().as_slice(),
        [Question::Select { multi: true, .. }]
    ));
    for rejected in [r#"["a","b","c","d","e"]"#, "[]", r#"["a",""]"#, r#""a""#] {
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
    let controller = Host::answering([Reply::Value(r#""  because  ""#)]);
    assert_eq!(
        ask_tool(&controller, &one("text", "")).await?,
        "Pick: because"
    );
    assert!(matches!(
        controller.asked().as_slice(),
        [Question::Text { .. }]
    ));
    let blank = Host::answering([Reply::Value(r#""   ""#)]);
    assert_eq!(ask_tool(&blank, &one("text", "")).await?, NO_ANSWER);
    Ok(())
}

#[tokio::test]
async fn single_answer_must_be_a_non_empty_string() -> TestResult {
    for rejected in [r#""""#, "7", r#"["Alpha"]"#] {
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
    let controller = Host::answering([Reply::Value(r#""Alpha""#)]);
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
    let controller = Host::answering([Reply::Value(r#""yes""#), Reply::Dismissed]);
    let text = ask_tool(&controller, &three_questions()).await?;
    assert_eq!(text, "One: yes\nUnanswered: Two, Three");
    assert_eq!(controller.asked().len(), 2);
    Ok(())
}

#[tokio::test]
async fn every_question_answered_yields_one_line_each() -> TestResult {
    let controller = Host::answering([
        Reply::Value(r#""a""#),
        Reply::Value(r#""b""#),
        Reply::Value(r#""c""#),
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
        (r#"{"questions":true}"#.to_owned(), "ask: invalid input"),
        (r#"{"nope":1}"#.to_owned(), "ask: invalid input"),
    ]
}

#[tokio::test]
async fn invalid_input_is_rejected_before_any_question_is_raised() -> TestResult {
    for (args, expected) in invalid_question_cases()
        .into_iter()
        .chain(invalid_value_cases())
    {
        let controller = Host::answering([Reply::Value(r#""Alpha""#)]);
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
