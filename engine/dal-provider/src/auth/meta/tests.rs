use super::{KEY_LIFETIME_SECS, Outcome, credential, outcome, request};
use crate::{OAuthCredential, SecretString};
use dal_core::Family;

const IDENTITY: &str = "meta-identity-token";

fn key_of(outcome: &Outcome) -> &str {
    match outcome {
        Outcome::Key(key) => key.expose(),
        other => panic!("expected a minted key, got {other:?}"),
    }
}

fn refused_of(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Refused(error) => format!("{error}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_minted_key_is_the_credential_and_keeps_the_identity_token() {
    let minted = outcome(
        Family::Responses,
        200,
        br#"{"api_key":"mkey-1"}"#,
        &[IDENTITY],
    );
    let key = key_of(&minted);
    assert_eq!(key, "mkey-1");

    let stored = credential(SecretString::from(key), SecretString::from(IDENTITY), 1_000);
    let OAuthCredential {
        access_token,
        refresh_token,
        expires_at,
        id_token,
        account_id,
        project_id,
    } = stored;
    assert_eq!(access_token.expose(), "mkey-1");
    assert_eq!(refresh_token.expose(), IDENTITY, "the renewal secret");
    assert_eq!(expires_at, Some(1_000 + KEY_LIFETIME_SECS));
    assert!(id_token.is_none() && account_id.is_none() && project_id.is_none());
}

#[test]
fn a_key_without_a_lifetime_counts_the_pinned_twenty_four_hours() {
    assert_eq!(KEY_LIFETIME_SECS, 24 * 60 * 60);
}

#[test]
fn a_dead_session_is_typed_before_the_body_is_read() {
    for status in [401, 403] {
        let body = format!(r#"{{"error":"token {IDENTITY} leaked"}}"#);
        let minted = outcome(Family::Responses, status, body.as_bytes(), &[IDENTITY]);
        assert!(
            matches!(minted, Outcome::SessionExpired),
            "{status} must end the session: {minted:?}"
        );
    }
}

#[test]
fn a_refused_mint_names_the_status_without_secrets() {
    let minted = outcome(
        Family::Responses,
        500,
        format!(r#"{{"detail":"upstream saw {IDENTITY}"}}"#).as_bytes(),
        &[IDENTITY],
    );
    let message = refused_of(&minted);
    assert!(!message.contains(IDENTITY), "{message} leaks the identity");
}

#[test]
fn a_mint_without_a_key_names_the_setup_page_when_it_is_trusted() {
    let minted = outcome(
        Family::Responses,
        200,
        br#"{"action_url":"https://meta.ai/device/setup"}"#,
        &[IDENTITY],
    );
    let message = refused_of(&minted);
    assert!(
        message.contains("https://meta.ai/device/setup"),
        "{message} must show the setup page"
    );

    // A plain `http` page, one with credentials, one smuggling a query or a
    // fragment, and a non-URL are never shown.
    for untrusted in [
        "http://meta.ai/setup",
        "https://user:pass@meta.ai/setup",
        "https://meta.ai/device/setup?step=1",
        "https://meta.ai/device/setup#step-1",
        "not a url",
    ] {
        let body = format!(r#"{{"action_url":"{untrusted}"}}"#);
        let minted = outcome(Family::Responses, 200, body.as_bytes(), &[IDENTITY]);
        let message = refused_of(&minted);
        assert!(
            !message.contains(untrusted),
            "{message} must not show {untrusted}"
        );
    }
}

#[test]
fn a_setup_page_smuggling_a_query_or_fragment_is_never_shown() {
    let minted = outcome(
        Family::Responses,
        200,
        br#"{"action_url":"https://example.invalid/setup?token=sensitive#private"}"#,
        &[IDENTITY],
    );
    let message = match &minted {
        Outcome::Refused(crate::ProviderError::TokenExchange { message, .. }) => message,
        other => panic!("a keyless mint is a TokenExchange refusal: {other:?}"),
    };
    for leaked in ["sensitive", "private", "?", "#"] {
        assert!(
            !message.contains(leaked),
            "the query/fragment must not reach the message"
        );
    }

    // Control: a plain page from the same host is shown.
    let minted = outcome(
        Family::Responses,
        200,
        br#"{"action_url":"https://example.invalid/setup"}"#,
        &[IDENTITY],
    );
    let message = refused_of(&minted);
    assert!(
        message.contains("https://example.invalid/setup"),
        "{message} must show the plain setup page"
    );
}

#[test]
fn an_empty_or_unparsable_mint_answer_is_a_refusal() {
    for body in [b"".as_slice(), br"not json".as_slice()] {
        let minted = outcome(Family::Responses, 200, body, &[IDENTITY]);
        assert!(
            matches!(minted, Outcome::Refused(_)),
            "{body:?} cannot mint: {minted:?}"
        );
    }
}

#[test]
fn the_mint_request_carries_a_sensitive_bearer_and_an_empty_typed_body() {
    let url: url::Url = "https://example.invalid/muse-code/key"
        .parse()
        .expect("the mint url parses");
    let built = request(&reqwest::Client::new(), &url, IDENTITY)
        .build()
        .expect("the mint request builds");
    let authorization = built
        .headers()
        .get(reqwest::header::AUTHORIZATION)
        .expect("a bearer header");
    assert_eq!(authorization, &format!("Bearer {IDENTITY}"));
    assert!(
        authorization.is_sensitive(),
        "the identity token rides a sensitive header"
    );
    assert_eq!(
        built
            .headers()
            .get("x-api-version")
            .expect("the pinned version"),
        "1.0.0"
    );
    assert_eq!(
        built.body().and_then(reqwest::Body::as_bytes),
        Some(br"{}".as_slice()),
        "the typed mint body serializes to an empty object"
    );
}
