// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

#[test]
fn the_model_reads_the_dalgona_manual_through_the_read_tool() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-manual-read")?;
    let steps = [
        support::tool_step("page", "read", r#"{"path":"dalgona://review"}"#),
        support::tool_step("index", "read", r#"{"path":"dalgona://"}"#),
        support::tool_step("miss", "read", r#"{"path":"dalgona://reveiw"}"#),
        support::text_step("done"),
    ];
    let journal = support::run_scripted_print(
        &scratch,
        "disabled_batteries = [\"judged\"]",
        &steps,
        "read the manual",
    )?;
    let results = support::journal_records(&journal, "tool_result");
    assert_eq!(results.len(), 3, "{journal}");
    let [page, index, miss] = results.as_slice() else {
        return Err("three read results".into());
    };
    assert!(page.contains(r#""error":false"#), "{page}");
    assert!(page.contains("# review"), "{page}");
    assert!(index.contains(r#""error":false"#), "{index}");
    assert!(index.contains("dalgona://batteries"), "{index}");
    assert!(miss.contains(r#""error":true"#), "{miss}");
    assert!(miss.contains("dalgona://review"), "{miss}");
    Ok(())
}
