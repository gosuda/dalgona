// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! `web_fetch` refuses private targets and bad URLs before any fetch.
use dalgona_gates::support;

/// Every battery other than `web` stays off so the scripted fetch below
/// drives exactly the web tool under test.
const WEB_ONLY: &str = "disabled_batteries = [\"ask\", \"history\", \"judged\", \"mcp\", \"orchestration\", \"procs\", \"quality\", \"review\", \"skills\", \"ttsr-rules\", \"work\"]";

/// Runs one scripted `web_fetch` call through the real binary and returns its
/// single `tool_result` record.
fn fetch(label: &str, url: &str) -> support::TestResult<String> {
    let scratch = support::Scratch::new(label)?;
    let args = format!("{{\"url\":\"{url}\"}}");
    let journal = support::run_scripted_print(
        &scratch,
        WEB_ONLY,
        &[
            support::tool_step("fetch-1", "web_fetch", &args),
            support::text_step("done"),
        ],
        "fetch the page",
    )?;
    let results = support::journal_records(&journal, "tool_result");
    assert_eq!(results.len(), 1, "{journal}");
    Ok(results[0].to_owned())
}

#[test]
fn web_fetch_refuses_private_targets_and_malformed_urls() -> support::TestResult<()> {
    // Loopback never leaves the process: the connect-time guard refuses it
    // with the private-address message, so no raw reply can flow through.
    let loopback = fetch("web-loopback-refused", "http://127.0.0.1/")?;
    assert!(
        loopback.contains("is a private address; fetch public URLs only"),
        "loopback is refused before fetching: {loopback}"
    );
    // A non-HTTP scheme is refused with its exact error, as is a bad URL.
    let scheme = fetch("web-bad-scheme", "ftp://example.com/gate")?;
    assert!(
        scheme.contains("unsupported URL scheme"),
        "unsupported schemes are refused: {scheme}"
    );
    let invalid = fetch("web-invalid-url", "::: not a url :::")?;
    assert!(
        invalid.contains("invalid URL:"),
        "malformed URLs are refused: {invalid}"
    );
    Ok(())
}
