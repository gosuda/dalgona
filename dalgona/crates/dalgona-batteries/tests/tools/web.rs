// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use super::common::{Host, Net, TestResult, locked, run_tool};
use dalgona_batteries::web::{WebConfig, web};

fn config(timeout_secs: u32) -> WebConfig {
    WebConfig {
        enabled: true,
        provider: "brave".into(),
        api_key_env: "BRAVE_API_KEY".into(),
        timeout_secs,
        max_redirects: 10,
        max_body_bytes: 2_097_152,
        max_markdown_bytes: 131_072,
    }
}

#[tokio::test]
async fn a_fetch_that_outlives_the_deadline_times_out_naming_the_url() -> TestResult {
    let host = Host::answering([]);
    *locked(&host.net) = Net::Hang;
    let extension = web(config(1))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://example.test/slow"}"#,
    )
    .await?;
    assert_eq!(
        text,
        "web_fetch timed out after 1s: http://example.test/slow"
    );
    Ok(())
}

#[tokio::test]
async fn an_unreachable_net_service_is_a_network_error_not_a_timeout() -> TestResult {
    let host = Host::answering([]);
    let extension = web(config(30))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://example.test/"}"#,
    )
    .await?;
    assert!(text.starts_with("web_fetch failed: "), "{text}");
    Ok(())
}

#[tokio::test]
async fn network_cancellation_interrupts_the_tool() -> TestResult {
    let host = Host::answering([]);
    *locked(&host.net) = Net::Cancel;
    let extension = web(config(30))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"http://example.test/"}"#,
    )
    .await?;
    assert_eq!(text, "interrupted");
    Ok(())
}

#[tokio::test]
async fn non_http_schemes_never_reach_the_net_service() -> TestResult {
    let host = Host::answering([]);
    *locked(&host.net) = Net::Hang;
    let extension = web(config(1))?;
    let text = run_tool(
        &extension,
        &host,
        "web_fetch",
        r#"{"url":"ftp://example.test/x"}"#,
    )
    .await?;
    assert_eq!(
        text,
        "unsupported URL scheme \"ftp\": only http and https are allowed"
    );
    Ok(())
}
