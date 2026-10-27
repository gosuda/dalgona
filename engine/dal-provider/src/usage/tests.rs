use std::cell::RefCell;
use std::io::ErrorKind;
use std::pin::pin;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::future::{Either, join, join_all, select};
use tokio::net::{TcpListener, TcpStream};

use sonic_rs::JsonValueMutTrait;

use super::*;
use crate::auth::credential::OAuthCredential;

const ACCESS: &str = "at-1";
const REFRESH: &str = "rt-secret";

const U1: &str = r#"{"account_id":"account-a","user_id":"user-a","plan_type":"free","rate_limit":{"allowed":false,"limit_reached":true},"rate_limit_upsell":{"banner_type":"luna_reserve","presentation":"dismissible","title":"You’re now using Luna, a faster model for simpler tasks.","description":"Add credits or upgrade to continue using the most advanced models.","ctas":[{"action":"add_credits","label":"Add credits"},{"action":"open_pricing_dialog","label":"Upgrade"}]}}"#;
const U5: &str = r#"{"account_id":"account-a","user_id":"user-a","plan_type":"plus","rate_limit":{"allowed":true,"limit_reached":false}"#;

fn u1_offer() -> Offer {
    Offer {
        after_switch_title: String::from(
            "You’re now using Luna, a faster model for simpler tasks.",
        ),
        after_switch_description: String::from(
            "Add credits or upgrade to continue using the most advanced models.",
        ),
        ctas: vec![
            Cta {
                label: String::from("Add credits"),
                url: String::from(CREDITS_URL),
            },
            Cta {
                label: String::from("Upgrade"),
                url: String::from(PLUS_URL),
            },
        ],
        blocked_model: None,
        normal_model: None,
    }
}

/// U1 with its `rate_limit_upsell` value replaced.
fn with_upsell(upsell: &str) -> String {
    let at = U1
        .find(r#""rate_limit_upsell":"#)
        .expect("U1 has an upsell");
    format!(r#"{}"rate_limit_upsell":{upsell}}}"#, &U1[..at])
}

/// U1 with banner member `key` set to the JSON text `value`.
fn banner_with(key: &str, value: &str) -> String {
    let mut banner: Value = sonic_rs::from_str(
        &U1[U1.find(r#""rate_limit_upsell":"#).expect("upsell") + 20..U1.len() - 1],
    )
    .expect("U1 banner parses");
    let value: Value = sonic_rs::from_str(value).expect("member value parses");
    banner
        .as_object_mut()
        .expect("banner object")
        .insert(key, value);
    with_upsell(&sonic_rs::to_string(&banner).expect("encode banner"))
}

fn decode(body: &str) -> UsageOutcome {
    decode_verdict("account-a", Some("user-a"), body)
}

fn u5_plus(extra: &str) -> String {
    format!("{U5},{extra}}}")
}

#[test]
fn u1_offer_matches_the_backend_copy_and_cta_urls() {
    assert_eq!(decode(U1), Ok(UsageVerdict::ReserveOffered(u1_offer())));
}

#[test]
fn cta_urls_follow_the_plan() {
    let upgrade = |plan: &str| {
        let body = U1.replace(r#""plan_type":"free""#, &format!(r#""plan_type":"{plan}""#));
        match decode(&body) {
            Ok(UsageVerdict::ReserveOffered(offer)) => offer.ctas,
            other => panic!("expected an offer, got {other:?}"),
        }
    };
    assert_eq!(upgrade("prolite")[1].url, PRO_2X_URL);
    assert_eq!(upgrade("plus")[1].url, PRO_URL);
    let team = upgrade("team");
    assert_eq!(
        team[0].url,
        "https://chatgpt.com/admin/billing?codex_credit_action=add_credits&account_id=account-a"
    );
    let body = U1
        .replace("account-a", "acct a&b")
        .replace(r#""plan_type":"free""#, r#""plan_type":"edu""#);
    match decode_verdict("acct a&b", Some("user-a"), &body) {
        Ok(UsageVerdict::ReserveOffered(offer)) => assert_eq!(
            offer.ctas[0].url,
            "https://chatgpt.com/admin/billing?codex_credit_action=add_credits&account_id=acct+a%26b"
        ),
        other => panic!("expected an offer, got {other:?}"),
    }
}

#[test]
fn another_account_or_user_or_no_user_is_no_change() {
    assert_eq!(
        decode_verdict("account-b", Some("user-a"), U1),
        Ok(UsageVerdict::NoChange)
    );
    assert_eq!(
        decode_verdict("account-a", Some("user-b"), U1),
        Ok(UsageVerdict::NoChange)
    );
    assert_eq!(
        decode_verdict("account-a", None, U1),
        Ok(UsageVerdict::NoChange)
    );
    let no_user = U1.replace(r#""user_id":"user-a","#, "");
    assert_eq!(decode(&no_user), Ok(UsageVerdict::NoChange));
}

#[test]
fn ordinary_usage_back_needs_an_explicit_permission() {
    assert_eq!(
        decode(&format!("{U5}}}")),
        Ok(UsageVerdict::OrdinaryUsageBack)
    );
    let base = U5.replace(
        r#","rate_limit":{"allowed":true,"limit_reached":false}"#,
        "",
    );
    for body in [
        format!("{base}}}"),
        format!(r#"{base},"rate_limit":null}}"#),
        format!(r#"{base},"rate_limit":{{"allowed":false,"limit_reached":true}}}}"#),
        u5_plus(r#""spend_control":{"reached":true}"#),
        u5_plus(r#""rate_limit_reached_type":{"type":"workspace_owner_usage_limit_reached"}"#),
        u5_plus(r#""rate_limit_upsell":{"unsupported":true}"#),
    ] {
        assert_eq!(decode(&body), Ok(UsageVerdict::NoChange), "{body}");
    }
    for body in [
        format!(
            r#"{base},"rate_limit":{{"allowed":false,"limit_reached":true}},"credits":{{"has_credits":true,"unlimited":false}}}}"#
        ),
        u5_plus(r#""rate_limit_reached_type":{"type":"unknown"}"#),
        u5_plus(r#""rate_limit_upsell":null"#),
    ] {
        assert_eq!(decode(&body), Ok(UsageVerdict::OrdinaryUsageBack), "{body}");
    }
}

#[test]
fn invalid_banners_are_no_change() {
    let xs = |n: usize| format!("\"{}\"", "x".repeat(n));
    let lines = |n: usize| format!("\"{}\"", "line\\n".repeat(n));
    let cases = [
        ("presentation", String::from("\"future_mode\"")),
        ("presentation", String::from("null")),
        ("title", String::from("\" \"")),
        ("title", xs(1025)),
        ("title", lines(4)),
        ("description", xs(4097)),
        ("description", lines(13)),
        ("blocked_model_slug", String::from("\"\"")),
        ("blocked_model_slug", String::from("\"bad\\nslug\"")),
        ("fallback_model_slugs", format!("[{}]", xs(257))),
        (
            "fallback_model_slugs",
            format!("[{}]", ["\"model\""; 17].join(",")),
        ),
        (
            "ctas",
            format!(
                "[{}]",
                [r#"{"action":"view_usage","label":"V"}"#; 9].join(",")
            ),
        ),
        ("ctas", String::from(r#"[{"action":"view_usage"}]"#)),
        ("banner_type", String::from("\"usage_limit\"")),
    ];
    for (key, value) in cases {
        assert_eq!(
            decode(&banner_with(key, &value)),
            Ok(UsageVerdict::NoChange),
            "{key} = {value}"
        );
    }
    let limits = [
        ("title", xs(1024)),
        ("title", lines(3)),
        ("description", lines(12)),
        ("presentation", String::from("\"inline\"")),
    ];
    for (key, value) in limits {
        assert!(
            matches!(
                decode(&banner_with(key, &value)),
                Ok(UsageVerdict::ReserveOffered(_))
            ),
            "{key} at its limit stays valid"
        );
    }
    let eight = format!(
        "[{}]",
        [r#"{"action":"view_usage","label":"View usage"}"#; 8].join(",")
    );
    match decode(&banner_with("ctas", &eight)) {
        Ok(UsageVerdict::ReserveOffered(offer)) => assert_eq!(offer.ctas.len(), 8),
        other => panic!("expected an offer, got {other:?}"),
    }
}

#[test]
fn ctas_drop_unknown_actions_and_bad_labels() {
    let ctas = r#"[{"action":"notify_owner","label":"Notify"},{"action":"add_credits","label":" "},{"action":"view_usage","label":"bad\u0007"},{"action":"view_usage","label":"View usage"}]"#;
    match decode(&banner_with("ctas", ctas)) {
        Ok(UsageVerdict::ReserveOffered(offer)) => assert_eq!(
            offer.ctas,
            vec![Cta {
                label: String::from("View usage"),
                url: String::from(USAGE_URL),
            }]
        ),
        other => panic!("expected an offer, got {other:?}"),
    }
}

#[test]
fn offer_models_and_title_controls() {
    let body = banner_with("blocked_model_slug", "\"gpt-6-sol\"").replace(
            r#""plan_type":"free","#,
            r#""plan_type":"free","additional_rate_limits":[{"limit_name":"codex_other","metered_feature":"codex_other","normal_model_slug":"gpt-6-sol"},{"limit_name":"gpt-reserve","metered_feature":"base_model_inference","normal_model_slug":"gpt-6-luna"}],"#,
        );
    match decode(&body) {
        Ok(UsageVerdict::ReserveOffered(offer)) => {
            assert_eq!(offer.blocked_model.as_deref(), Some("gpt-6-sol"));
            assert_eq!(offer.normal_model.as_deref(), Some("gpt-6-luna"));
        }
        other => panic!("expected an offer, got {other:?}"),
    }
    match decode(&banner_with("title", r#""You’re now\u0007 using\nLuna""#)) {
        Ok(UsageVerdict::ReserveOffered(offer)) => {
            assert_eq!(offer.after_switch_title, "You’re now using\nLuna");
        }
        other => panic!("expected an offer, got {other:?}"),
    }
}

#[test]
fn banner_limits_admit_the_limit_and_refuse_one_more_counted_in_bytes() {
    let xs = |n: usize| format!("\"{}\"", "x".repeat(n));
    let curly = |n: usize, tail: &str| format!("\"{}{tail}\"", "\u{2019}".repeat(n));
    let slugs = |n: usize| format!("[{}]", ["\"model\""; 17][..n].join(","));
    let offered = |key: &str, value: &str| {
        matches!(
            decode(&banner_with(key, value)),
            Ok(UsageVerdict::ReserveOffered(_))
        )
    };
    let mut problems = Vec::new();
    for (name, key, value, admitted) in [
        ("description of 4096 bytes", "description", xs(4096), true),
        ("description of 4097 bytes", "description", xs(4097), false),
        (
            "description of 4096 bytes in curly quotes",
            "description",
            curly(1365, "x"),
            true,
        ),
        (
            "description of 4098 bytes in 1366 curly quotes",
            "description",
            curly(1366, ""),
            false,
        ),
        (
            "title of 1024 bytes in curly quotes",
            "title",
            curly(341, "x"),
            true,
        ),
        (
            "title of 1026 bytes in 342 curly quotes",
            "title",
            curly(342, ""),
            false,
        ),
        (
            "blocked slug of 256 bytes",
            "blocked_model_slug",
            xs(256),
            true,
        ),
        (
            "blocked slug of 257 bytes",
            "blocked_model_slug",
            xs(257),
            false,
        ),
        (
            "blocked slug of 256 bytes in curly quotes",
            "blocked_model_slug",
            curly(85, "x"),
            true,
        ),
        (
            "blocked slug of 258 bytes in 86 curly quotes",
            "blocked_model_slug",
            curly(86, ""),
            false,
        ),
        (
            "fallback slug of 256 bytes",
            "fallback_model_slugs",
            format!("[{}]", xs(256)),
            true,
        ),
        ("16 fallback slugs", "fallback_model_slugs", slugs(16), true),
        (
            "17 fallback slugs",
            "fallback_model_slugs",
            slugs(17),
            false,
        ),
    ] {
        if offered(key, &value) != admitted {
            problems.push(format!("{name}: expected admitted = {admitted}"));
        }
    }
    for (label, kept) in [(256, true), (257, false)] {
        let ctas = format!(
            r#"[{{"action":"view_usage","label":"{}"}}]"#,
            "L".repeat(label)
        );
        match decode(&banner_with("ctas", &ctas)) {
            Ok(UsageVerdict::ReserveOffered(offer)) if offer.ctas.len() == usize::from(kept) => {}
            other => problems.push(format!(
                "label of {label} bytes: expected kept = {kept}, got {other:?}"
            )),
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn account_and_user_must_match_the_caller_exactly() {
    let named = |account: &str, user: &str| {
        U1.replace(
            r#""account_id":"account-a""#,
            &format!(r#""account_id":{account}"#),
        )
        .replace(r#""user_id":"user-a""#, &format!(r#""user_id":{user}"#))
    };
    let mut problems = Vec::new();
    for (account, user) in [
        (r#""account-ab""#, r#""user-a""#),
        (r#""account-""#, r#""user-a""#),
        (r#""Account-A""#, r#""user-a""#),
        (r#""account-a ""#, r#""user-a""#),
        (r#"" account-a""#, r#""user-a""#),
        ("5", r#""user-a""#),
        ("null", r#""user-a""#),
        (r#""account-a""#, r#""user-ab""#),
        (r#""account-a""#, r#""USER-A""#),
        (r#""account-a""#, r#""user-a ""#),
        (r#""account-a""#, "5"),
        (r#""account-a""#, "null"),
    ] {
        let verdict = decode(&named(account, user));
        if verdict != Ok(UsageVerdict::NoChange) {
            problems.push(format!("body account {account} user {user}: {verdict:?}"));
        }
    }
    assert_eq!(
        decode(&named(r#""account-a""#, r#""user-a""#)),
        Ok(UsageVerdict::ReserveOffered(u1_offer())),
        "the exact pair still gets the offer"
    );
    for (account, user) in [
        ("account-ab", "user-a"),
        ("account-", "user-a"),
        ("account-a", "user-ab"),
        ("account-a", ""),
    ] {
        let verdict = decode_verdict(account, Some(user), U1);
        if verdict != Ok(UsageVerdict::NoChange) {
            problems.push(format!("caller {account} / {user}: {verdict:?}"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn each_input_of_the_ordinary_usage_rule_decides_on_its_own() {
    let base = U5.replace(
        r#","rate_limit":{"allowed":true,"limit_reached":false}"#,
        "",
    );
    let with = |allowed: &str, extra: &str| {
        format!(r#"{base},"rate_limit":{{"allowed":{allowed}}}{extra}}}"#)
    };
    let mut problems = Vec::new();
    let mut expect = |name: String, body: String, verdict: UsageVerdict| {
        let got = decode(&body);
        if got != Ok(verdict.clone()) {
            problems.push(format!("{name}: expected {verdict:?}, got {got:?}"));
        }
    };
    expect(
        String::from("allowed false, unlimited credits"),
        with(
            "false",
            r#","credits":{"has_credits":false,"unlimited":true}"#,
        ),
        UsageVerdict::OrdinaryUsageBack,
    );
    expect(
        String::from("allowed false, neither credit flag"),
        with(
            "false",
            r#","credits":{"has_credits":false,"unlimited":false}"#,
        ),
        UsageVerdict::NoChange,
    );
    expect(
        String::from("allowed true, spend control not reached"),
        with("true", r#","spend_control":{"reached":false}"#),
        UsageVerdict::OrdinaryUsageBack,
    );
    expect(
        String::from("allowed given as text"),
        with(r#""true""#, ""),
        UsageVerdict::NoChange,
    );
    for blocking in [
        "rate_limit_reached",
        "workspace_owner_credits_depleted",
        "workspace_member_credits_depleted",
        "workspace_owner_usage_limit_reached",
        "workspace_member_usage_limit_reached",
    ] {
        expect(
            format!("allowed true but {blocking}"),
            with(
                "true",
                &format!(r#","rate_limit_reached_type":{{"type":"{blocking}"}}"#),
            ),
            UsageVerdict::NoChange,
        );
    }
    for other in ["rate_limit_reached_soon", "RATE_LIMIT_REACHED", ""] {
        expect(
            format!("allowed true with the non-blocking type {other:?}"),
            with(
                "true",
                &format!(r#","rate_limit_reached_type":{{"type":"{other}"}}"#),
            ),
            UsageVerdict::OrdinaryUsageBack,
        );
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn only_workspace_plans_buy_credits_through_the_admin_page() {
    let body_for = |action: &str, plan: Option<&str>| {
        let ctas = format!(r#"[{{"action":"{action}","label":"Buy"}}]"#);
        let body = banner_with("ctas", &ctas);
        match plan {
            Some(plan) => {
                body.replace(r#""plan_type":"free""#, &format!(r#""plan_type":"{plan}""#))
            }
            None => body.replace(r#""plan_type":"free","#, ""),
        }
    };
    let url_of = |body: String| match decode(&body) {
        Ok(UsageVerdict::ReserveOffered(offer)) => offer.ctas.into_iter().next().map(|cta| cta.url),
        other => panic!("expected an offer, got {other:?}"),
    };
    let admin = format!("{BILLING_URL}account-a");
    let mut problems = Vec::new();
    for action in ["add_credits", "buy_credits"] {
        for plan in [
            "team",
            "self_serve_business_prolite",
            "self_serve_business_usage_based",
            "business",
            "ent26",
            "enterprise_cbp_automation",
            "enterprise_cbp_usage_based",
            "enterprise",
            "edu",
            "education",
            "edu_plus",
            "edu_pro",
        ] {
            let url = url_of(body_for(action, Some(plan)));
            if url.as_deref() != Some(admin.as_str()) {
                problems.push(format!("{action} on {plan}: {url:?}"));
            }
        }
        for plan in [
            Some("free"),
            Some("plus"),
            Some("pro"),
            Some("prolite"),
            Some("Team"),
            Some("team "),
            Some("enterprise2"),
            Some(""),
            None,
        ] {
            let url = url_of(body_for(action, plan));
            if url.as_deref() != Some(CREDITS_URL) {
                problems.push(format!("{action} on {plan:?}: {url:?}"));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn the_normal_model_is_the_first_reserve_entry_and_only_when_it_is_a_non_empty_string() {
    let normal_model = |limits: &str| {
        let body = U1.replace(
            r#""plan_type":"free","#,
            &format!(r#""plan_type":"free","additional_rate_limits":{limits},"#),
        );
        match decode(&body) {
            Ok(UsageVerdict::ReserveOffered(offer)) => offer.normal_model,
            other => panic!("expected an offer, got {other:?}"),
        }
    };
    let mut problems = Vec::new();
    for (name, limits, expected) in [
        (
            "first entry wins",
            r#"[{"limit_name":"gpt-reserve","normal_model_slug":"first"},{"limit_name":"gpt-reserve","normal_model_slug":"second"}]"#,
            Some("first"),
        ),
        (
            "an empty first slug is not skipped",
            r#"[{"limit_name":"gpt-reserve","normal_model_slug":""},{"limit_name":"gpt-reserve","normal_model_slug":"gpt-6-luna"}]"#,
            None,
        ),
        (
            "a near-miss limit name",
            r#"[{"limit_name":"gpt-reserve-2","normal_model_slug":"x"},{"limit_name":"GPT-RESERVE","normal_model_slug":"y"}]"#,
            None,
        ),
        (
            "a slug that is not a string",
            r#"[{"limit_name":"gpt-reserve","normal_model_slug":5}]"#,
            None,
        ),
        (
            "not an array",
            r#"{"limit_name":"gpt-reserve","normal_model_slug":"x"}"#,
            None,
        ),
    ] {
        let got = normal_model(limits);
        if got.as_deref() != expected {
            problems.push(format!("{name}: expected {expected:?}, got {got:?}"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn server_messages_cut_at_300_bytes_on_a_boundary_and_at_either_line_break() {
    let message_of = |text: &str| {
        let body = format!(
            r#"{{"error":{{"message":{}}}}}"#,
            sonic_rs::to_string(text).unwrap()
        );
        match map_codex_error(403, &body, LUNA_RESERVE_MODEL) {
            Some(ProviderError::ReserveUnavailable { message, .. }) => message,
            other => panic!("expected a reserve refusal, got {other:?}"),
        }
    };
    let mut problems = Vec::new();
    for (name, text, expected) in [
        ("300 ASCII bytes", "a".repeat(300), "a".repeat(300)),
        ("301 ASCII bytes", "a".repeat(301), "a".repeat(300)),
        (
            "150 two-byte characters",
            "\u{e9}".repeat(150),
            "\u{e9}".repeat(150),
        ),
        (
            "151 two-byte characters",
            "\u{e9}".repeat(151),
            "\u{e9}".repeat(150),
        ),
        (
            "padding is trimmed before the cut",
            format!("  {}  ", "a".repeat(300)),
            "a".repeat(300),
        ),
        (
            "CRLF ends the line",
            String::from("first\r\nsecond"),
            String::from("first"),
        ),
        (
            "CR ends the line",
            String::from("first\rsecond"),
            String::from("first"),
        ),
        (
            "LF ends the line",
            String::from("first\nsecond"),
            String::from("first"),
        ),
    ] {
        let got = message_of(&text);
        if got != expected {
            problems.push(format!(
                "{name}: got {} bytes {:?}",
                got.len(),
                got.chars().take(12).collect::<String>()
            ));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn only_the_snake_case_upsell_member_is_read() {
    let camel = U1.replace("rate_limit_upsell", "rateLimitUpsell");
    assert_eq!(decode(&camel), Ok(UsageVerdict::NoChange));
}

#[test]
fn non_object_bodies_are_bad_bodies() {
    for body in ["[]", "", "not json", "\"text\"", "null"] {
        assert_eq!(
            decode(body),
            Err(UsageCheckReason::NotJsonObject),
            "{body:?}"
        );
    }
}

fn mapped(status: u16, body: &str, model: &str) -> Option<(String, Option<String>)> {
    map_codex_error(status, body, model).map(|error| (error.to_string(), error.fix()))
}

#[test]
fn codex_usage_errors_map_without_retry() {
    let limit = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"pro","resets_at":1738888888}}"#;
    let error = map_codex_error(429, limit, "gpt-6-sol");
    assert!(matches!(
        &error,
        Some(ProviderError::UsageLimit { model, message })
            if model == "gpt-6-sol" && message == "The usage limit has been reached"
    ));
    assert_eq!(
        mapped(429, limit, "gpt-6-sol")
            .map(|(text, _)| text)
            .as_deref(),
        Some("usage limit reached: The usage limit has been reached")
    );
    assert_eq!(
        mapped(429, limit, LUNA_RESERVE_MODEL)
            .map(|(text, _)| text)
            .as_deref(),
        Some("Luna Reserve usage limit reached: The usage limit has been reached")
    );
    let frame = r#"{"type":"error","status":429,"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"pro","resets_at":1738888888}}"#;
    assert_eq!(
        mapped(429, frame, "gpt-6-sol"),
        mapped(429, limit, "gpt-6-sol")
    );
    assert_eq!(
        mapped(
            429,
            r#"{"error":{"type":"usage_not_included"}}"#,
            "gpt-6-sol"
        )
        .map(|(text, _)| text)
        .as_deref(),
        Some("this ChatGPT plan does not include Codex usage: the server gave no reason")
    );
    assert!(map_codex_error(429, r#"{"error":{"type":"rate_limit_error"}}"#, "m").is_none());
    assert!(map_codex_error(429, "not json", LUNA_RESERVE_MODEL).is_none());
    assert!(!error.expect("mapped").retryable_by_loop());
}

#[test]
fn reserve_refusals_are_reserve_unavailable_with_the_fix() {
    let detail = r#"{"detail":"The 'gpt-reserve' model is not supported when using Codex with a ChatGPT account."}"#;
    let fix = "Luna Reserve opens only when the included usage of your ChatGPT plan runs out. Switch to another model to continue.";
    assert_eq!(
        mapped(400, detail, LUNA_RESERVE_MODEL),
        Some((
            String::from(
                "Luna Reserve is not available for this account: The 'gpt-reserve' model is not supported when using Codex with a ChatGPT account."
            ),
            Some(String::from(fix))
        ))
    );
    let not_found = r#"{"error":{"message":"Model not found gpt-reserve","type":"invalid_request_error","param":"model","code":null}}"#;
    assert!(matches!(
        map_codex_error(404, not_found, LUNA_RESERVE_MODEL),
        Some(ProviderError::ReserveUnavailable { status: 404, message })
            if message == "Model not found gpt-reserve"
    ));
    assert!(matches!(
        map_codex_error(403, "", LUNA_RESERVE_MODEL),
        Some(ProviderError::ReserveUnavailable { status: 403, message }) if message == NO_REASON
    ));
    assert!(map_codex_error(400, detail, "gpt-6-sol").is_none());
    assert!(map_codex_error(404, not_found, "gpt-6-sol").is_none());
    assert!(map_codex_error(500, not_found, LUNA_RESERVE_MODEL).is_none());
}

#[test]
fn server_messages_keep_one_trimmed_line_of_300_bytes() {
    let long = format!(
        r#"{{"error":{{"message":"  {}é tail\nnext  "}}}}"#,
        "m".repeat(299)
    );
    assert!(matches!(
        map_codex_error(403, &long, LUNA_RESERVE_MODEL),
        Some(ProviderError::ReserveUnavailable { message, .. }) if message == "m".repeat(299)
    ));
    let blank = r#"{"error":{"message":"   "},"detail":" why "}"#;
    assert!(matches!(
        map_codex_error(404, blank, LUNA_RESERVE_MODEL),
        Some(ProviderError::ReserveUnavailable { message, .. }) if message == "why"
    ));
}

// Checker cases: a loopback server driven on the test task, a host task
// list instead of a runtime spawn, and a clock the test moves.

enum Reply {
    Json(u16, String),
    Stall,
}

struct TestClock {
    base: Instant,
    offset_ms: Arc<AtomicU64>,
}

impl TestClock {
    fn new() -> Self {
        Self {
            base: Instant::now(),
            offset_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    fn clock(&self) -> Clock {
        let (base, offset) = (self.base, Arc::clone(&self.offset_ms));
        Arc::new(move || base + Duration::from_millis(offset.load(Ordering::SeqCst)))
    }

    fn set(&self, offset: Duration) {
        let millis = u64::try_from(offset.as_millis()).expect("small offset");
        self.offset_ms.store(millis, Ordering::SeqCst);
    }
}

fn jwt(payload: &str) -> String {
    format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(payload))
}

fn codex(payload: &str) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from(ACCESS),
        refresh_token: SecretString::from(REFRESH),
        expires_at: None,
        id_token: Some(jwt(payload)),
        account_id: Some(String::from("account-a")),
        project_id: None,
    })
}

fn account_a() -> Credential {
    codex(
        r#"{"https://api.openai.com/auth":{"chatgpt_user_id":"user-a","chatgpt_account_id":"account-a"}}"#,
    )
}

async fn listen() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    (
        listener,
        format!("http://127.0.0.1:{port}/backend-api/codex/"),
    )
}

fn checker(base: &str, timeout: Duration, clock: &TestClock) -> UsageChecker {
    UsageChecker::with_timing(
        LazyClient::default(),
        base,
        "dalgon/test",
        timeout,
        clock.clock(),
    )
    .expect("loopback base")
}

async fn read_head(stream: &TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if data.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8_lossy(&data).into_owned();
        }
        stream.readable().await.expect("readable");
        match stream.try_read(&mut chunk) {
            Ok(0) => panic!("client closed before the request ended"),
            Ok(read) => data.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => panic!("read request: {error}"),
        }
    }
}

async fn write_reply(stream: &TcpStream, status: u16, body: &str) {
    let text = format!(
        "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut bytes = text.as_bytes();
    while !bytes.is_empty() {
        stream.writable().await.expect("writable");
        match stream.try_write(bytes) {
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(error) => panic!("write reply: {error}"),
        }
    }
}

/// Answers one connection per reply in order, records each request head,
/// then never completes.
async fn serve(listener: TcpListener, replies: Vec<Reply>, seen: &std::sync::Mutex<Vec<String>>) {
    let mut held = Vec::new();
    for reply in replies {
        let (stream, _) = listener.accept().await.expect("accept");
        let request_head = read_head(&stream).await;
        seen.lock().expect("record").push(request_head);
        match reply {
            Reply::Json(status, body) => write_reply(&stream, status, &body).await,
            Reply::Stall => held.push(stream),
        }
    }
    std::future::pending::<()>().await;
}

async fn with_server<T>(
    listener: TcpListener,
    replies: Vec<Reply>,
    work: impl Future<Output = T>,
) -> (T, Vec<String>) {
    let seen = std::sync::Mutex::new(Vec::new());
    let output = match select(pin!(work), pin!(serve(listener, replies, &seen))).await {
        Either::Left((output, _)) => output,
        Either::Right(((), _)) => panic!("the server stopped"),
    };
    (output, seen.into_inner().expect("unpoisoned"))
}

#[tokio::test]
async fn sixty_four_callers_share_one_request_then_the_cache_expires() {
    let (listener, base) = listen().await;
    let clock = TestClock::new();
    let checker = checker(&base, USAGE_TIMEOUT, &clock);
    let host = RefCell::new(Vec::<UsageTask>::new());
    let spawn = |task: UsageTask| host.borrow_mut().push(task);
    let credential = account_a();

    let waits: Vec<_> = (0..64)
        .map(|_| checker.check(&credential, &spawn))
        .collect();
    assert_eq!(host.borrow().len(), 1, "one request task for 64 callers");
    let tasks = host.take();
    let replies = vec![
        Reply::Json(200, String::from(U1)),
        Reply::Json(200, format!("{U5}}}")),
    ];
    let seen = std::sync::Mutex::new(Vec::new());
    let mut server = pin!(serve(listener, replies, &seen));
    let results = match select(
        pin!(join(join_all(tasks), join_all(waits))),
        server.as_mut(),
    )
    .await
    {
        Either::Left(((_, results), _)) => results,
        Either::Right(((), _)) => panic!("the server stopped"),
    };
    assert_eq!(results.len(), 64);
    for result in results {
        assert_eq!(result, Ok(UsageVerdict::ReserveOffered(u1_offer())));
    }
    {
        let heads = seen.lock().expect("read");
        assert_eq!(heads.len(), 1);
        let head = heads[0].to_ascii_lowercase();
        assert!(
            head.starts_with("get /backend-api/wham/usage http/1.1\r\n"),
            "{head}"
        );
        for line in [
            "authorization: bearer at-1",
            "chatgpt-account-id: account-a",
            "x-openai-codex-luna-reserve: 1",
            "accept: application/json",
            "user-agent: dalgon/test",
        ] {
            assert!(
                head.contains(&format!("\r\n{line}\r\n")),
                "{line} missing in {head}"
            );
        }
    }

    clock.set(Duration::from_secs(4));
    let cached = checker.check(&credential, &spawn).await;
    assert_eq!(cached, Ok(UsageVerdict::ReserveOffered(u1_offer())));
    assert!(
        host.borrow().is_empty(),
        "a 4 s old outcome sends no request"
    );

    clock.set(Duration::from_secs(6));
    let wait = checker.check(&credential, &spawn);
    let tasks = host.take();
    assert_eq!(tasks.len(), 1, "a 6 s old outcome starts one request");
    let fresh = match select(pin!(join(join_all(tasks), wait)), server.as_mut()).await {
        Either::Left(((_, fresh), _)) => fresh,
        Either::Right(((), _)) => panic!("the server stopped"),
    };
    assert_eq!(fresh, Ok(UsageVerdict::OrdinaryUsageBack));
    assert_eq!(seen.lock().expect("read").len(), 2);
}

#[tokio::test]
async fn api_keys_fedramp_and_non_codex_sign_ins_send_nothing() {
    let clock = TestClock::new();
    let checker = checker(
        "https://chatgpt.com/backend-api/codex",
        USAGE_TIMEOUT,
        &clock,
    );
    let spawned = RefCell::new(0_u32);
    let spawn = |_task: UsageTask| *spawned.borrow_mut() += 1;
    let fedramp = codex(
        r#"{"https://api.openai.com/auth":{"chatgpt_user_id":"user-a","chatgpt_account_id":"account-a","chatgpt_account_is_fedramp":true}}"#,
    );
    let anthropic = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from(ACCESS),
        refresh_token: SecretString::from(REFRESH),
        expires_at: None,
        id_token: None,
        account_id: None,
        project_id: None,
    });
    let key = Credential::ApiKey {
        key: SecretString::from("sk-test"),
    };
    for credential in [fedramp, anthropic, key, Credential::None] {
        assert_eq!(
            checker.check(&credential, &spawn).await,
            Ok(UsageVerdict::NoChange)
        );
    }
    assert_eq!(*spawned.borrow(), 0);
    assert_eq!(
        checker.url.as_str(),
        "https://chatgpt.com/backend-api/wham/usage"
    );
}

#[tokio::test]
async fn a_silent_server_times_out_and_the_error_is_cached() {
    let (listener, base) = listen().await;
    let clock = TestClock::new();
    let checker = checker(&base, Duration::from_millis(300), &clock);
    let host = RefCell::new(Vec::<UsageTask>::new());
    let spawn = |task: UsageTask| host.borrow_mut().push(task);
    let credential = account_a();
    let wait = checker.check(&credential, &spawn);
    let tasks = host.take();
    let ((_, outcome), seen) =
        with_server(listener, vec![Reply::Stall], join(join_all(tasks), wait)).await;
    assert_eq!(outcome, Err(UsageCheckReason::Timeout));
    assert_eq!(seen.len(), 1);
    let reason = outcome.expect_err("timeout");
    assert_eq!(
        ProviderError::UsageCheck { reason }.to_string(),
        "usage failed: no reply within 15 s"
    );
    assert_eq!(
        checker.check(&credential, &spawn).await,
        Err(UsageCheckReason::Timeout)
    );
    assert!(host.borrow().is_empty(), "an error is reused for 5 s");
}

#[tokio::test]
async fn error_status_keeps_the_body_without_secrets() {
    let (listener, base) = listen().await;
    let clock = TestClock::new();
    let checker = checker(&base, USAGE_TIMEOUT, &clock);
    let host = RefCell::new(Vec::<UsageTask>::new());
    let spawn = |task: UsageTask| host.borrow_mut().push(task);
    let wait = checker.check(&account_a(), &spawn);
    let tasks = host.take();
    let body = format!("token {ACCESS} and {REFRESH} rejected\nsecond line");
    let ((_, outcome), _) = with_server(
        listener,
        vec![Reply::Json(401, body)],
        join(join_all(tasks), wait),
    )
    .await;
    assert_eq!(
        outcome,
        Err(UsageCheckReason::Status {
            status: 401,
            message: String::from("token <redacted> and <redacted> rejected\nsecond line"),
        })
    );
    let reason = outcome.expect_err("status");
    assert_eq!(
        ProviderError::UsageCheck { reason }.to_string(),
        "usage failed: 401 token <redacted> and <redacted> rejected"
    );
}

#[tokio::test]
async fn a_refused_connection_is_a_transport_reason() {
    let (listener, base) = listen().await;
    drop(listener);
    let clock = TestClock::new();
    let checker = checker(&base, USAGE_TIMEOUT, &clock);
    let host = RefCell::new(Vec::<UsageTask>::new());
    let spawn = |task: UsageTask| host.borrow_mut().push(task);
    let wait = checker.check(&account_a(), &spawn);
    let tasks = host.take();
    let (_, outcome) = join(join_all(tasks), wait).await;
    match outcome {
        Err(UsageCheckReason::Transport { reason }) => {
            assert_ne!(reason, "");
            assert!(
                !reason.contains(ACCESS) && !reason.contains(REFRESH),
                "{reason}"
            );
            let text = ProviderError::UsageCheck {
                reason: UsageCheckReason::Transport { reason },
            }
            .to_string();
            assert!(text.starts_with("usage failed: "), "{text}");
        }
        other => panic!("expected a transport reason, got {other:?}"),
    }
}

fn account_b() -> Credential {
    codex(
        r#"{"https://api.openai.com/auth":{"chatgpt_user_id":"user-b","chatgpt_account_id":"account-b"}}"#,
    )
}

#[tokio::test]
async fn a_cached_outcome_is_reused_until_exactly_five_seconds_old() {
    let (listener, base) = listen().await;
    let clock = TestClock::new();
    let checker = checker(&base, USAGE_TIMEOUT, &clock);
    let host = RefCell::new(Vec::<UsageTask>::new());
    let spawn = |task: UsageTask| host.borrow_mut().push(task);
    let credential = account_a();
    let seen = std::sync::Mutex::new(Vec::new());
    let replies = vec![
        Reply::Json(200, String::from(U1)),
        Reply::Json(200, format!("{U5}}}")),
    ];
    let mut server = pin!(serve(listener, replies, &seen));

    let wait = checker.check(&credential, &spawn);
    let tasks = host.take();
    assert_eq!(tasks.len(), 1);
    let first = match select(pin!(join(join_all(tasks), wait)), server.as_mut()).await {
        Either::Left(((_, first), _)) => first,
        Either::Right(((), _)) => panic!("the server stopped"),
    };
    assert_eq!(first, Ok(UsageVerdict::ReserveOffered(u1_offer())));

    clock.set(Duration::from_millis(4999));
    assert_eq!(checker.check(&credential, &spawn).await, first);
    assert!(host.borrow().is_empty(), "4999 ms old is still reused");

    clock.set(Duration::from_millis(5000));
    let wait = checker.check(&credential, &spawn);
    let tasks = host.take();
    assert_eq!(tasks.len(), 1, "an outcome exactly 5 s old is expired");
    let second = match select(pin!(join(join_all(tasks), wait)), server.as_mut()).await {
        Either::Left(((_, second), _)) => second,
        Either::Right(((), _)) => panic!("the server stopped"),
    };
    assert_eq!(second, Ok(UsageVerdict::OrdinaryUsageBack));
    assert_eq!(seen.lock().expect("read").len(), 2);
}

#[tokio::test]
async fn two_accounts_never_share_a_cached_outcome_or_a_request() {
    let (listener, base) = listen().await;
    let clock = TestClock::new();
    let checker = checker(&base, USAGE_TIMEOUT, &clock);
    let host = RefCell::new(Vec::<UsageTask>::new());
    let spawn = |task: UsageTask| host.borrow_mut().push(task);
    let (a, b) = (account_a(), account_b());
    let body_b = format!(
        "{}}}",
        U5.replace("account-a", "account-b")
            .replace("user-a", "user-b")
    );
    let seen = std::sync::Mutex::new(Vec::new());
    let replies = vec![Reply::Json(200, String::from(U1)), Reply::Json(200, body_b)];
    let mut server = pin!(serve(listener, replies, &seen));

    let wait = checker.check(&a, &spawn);
    let tasks = host.take();
    assert_eq!(tasks.len(), 1);
    let first = match select(pin!(join(join_all(tasks), wait)), server.as_mut()).await {
        Either::Left(((_, first), _)) => first,
        Either::Right(((), _)) => panic!("the server stopped"),
    };
    assert_eq!(first, Ok(UsageVerdict::ReserveOffered(u1_offer())));

    let wait = checker.check(&b, &spawn);
    let tasks = host.take();
    assert_eq!(
        tasks.len(),
        1,
        "account-b must not read the outcome cached for account-a"
    );
    let second = match select(pin!(join(join_all(tasks), wait)), server.as_mut()).await {
        Either::Left(((_, second), _)) => second,
        Either::Right(((), _)) => panic!("the server stopped"),
    };
    assert_eq!(second, Ok(UsageVerdict::OrdinaryUsageBack));

    assert_eq!(
        checker.check(&a, &spawn).await,
        Ok(UsageVerdict::ReserveOffered(u1_offer()))
    );
    assert_eq!(
        checker.check(&b, &spawn).await,
        Ok(UsageVerdict::OrdinaryUsageBack)
    );
    assert!(
        host.borrow().is_empty(),
        "both accounts keep their own cache"
    );
    let heads = seen.lock().expect("read");
    assert_eq!(heads.len(), 2);
    assert!(
        heads[0]
            .to_ascii_lowercase()
            .contains("\r\nchatgpt-account-id: account-a\r\n"),
        "{}",
        heads[0]
    );
    assert!(
        heads[1]
            .to_ascii_lowercase()
            .contains("\r\nchatgpt-account-id: account-b\r\n"),
        "{}",
        heads[1]
    );
}

#[tokio::test]
async fn error_bodies_never_echo_the_id_token_or_the_account_id() {
    let (listener, base) = listen().await;
    let clock = TestClock::new();
    let checker = checker(&base, USAGE_TIMEOUT, &clock);
    let host = RefCell::new(Vec::<UsageTask>::new());
    let spawn = |task: UsageTask| host.borrow_mut().push(task);
    let payload = r#"{"https://api.openai.com/auth":{"chatgpt_user_id":"user-a","chatgpt_account_id":"account-a"}}"#;
    let wait = checker.check(&codex(payload), &spawn);
    let tasks = host.take();
    let body = format!("jwt {} acct account-a", jwt(payload));
    let ((_, outcome), _) = with_server(
        listener,
        vec![Reply::Json(403, body)],
        join(join_all(tasks), wait),
    )
    .await;
    assert_eq!(
        outcome,
        Err(UsageCheckReason::Status {
            status: 403,
            message: String::from("jwt <redacted> acct <redacted>"),
        })
    );
}

#[tokio::test]
async fn a_dropped_caller_leaves_the_request_to_the_others() {
    let (listener, base) = listen().await;
    let clock = TestClock::new();
    let checker = checker(&base, USAGE_TIMEOUT, &clock);
    let host = RefCell::new(Vec::<UsageTask>::new());
    let spawn = |task: UsageTask| host.borrow_mut().push(task);
    let credential = account_a();
    let first = checker.check(&credential, &spawn);
    let second = checker.check(&credential, &spawn);
    drop(first);
    let tasks = host.take();
    assert_eq!(tasks.len(), 1);
    let ((_, outcome), seen) = with_server(
        listener,
        vec![Reply::Json(200, String::from(U1))],
        join(join_all(tasks), second),
    )
    .await;
    assert_eq!(outcome, Ok(UsageVerdict::ReserveOffered(u1_offer())));
    assert_eq!(seen.len(), 1);
}

#[tokio::test]
async fn a_dropped_host_task_releases_waiters_and_the_next_call_retries() {
    let clock = TestClock::new();
    let checker = checker(
        "https://chatgpt.com/backend-api/codex",
        USAGE_TIMEOUT,
        &clock,
    );
    let host = RefCell::new(Vec::<UsageTask>::new());
    let spawn = |task: UsageTask| host.borrow_mut().push(task);
    let credential = account_a();
    let wait = checker.check(&credential, &spawn);
    drop(host.take());
    assert_eq!(
        wait.await,
        Err(UsageCheckReason::Transport {
            reason: String::from(CANCELLED),
        })
    );
    drop(checker.check(&credential, &spawn));
    assert_eq!(host.borrow().len(), 1, "a cancelled request is not cached");
}
