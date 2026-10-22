use dal_core::ThinkingLevel::{High, Low, Max, Medium, Minimal};

use super::*;
use crate::family::ollama::live_fixtures::{
    BEAM, DEEPSEEK_V41, GEMMA_4, GLM_5_2, GPT_OSS, LOCAL_GEMMA3, LOCAL_QWEN3, MINIMAX_M2_7,
    MISTRAL_LARGE_4, NEMOTRON_ULTRA,
};

fn show(body: &str) -> OllamaShow {
    decode_show(body.as_bytes()).expect("a show body decodes")
}

#[test]
fn the_show_body_names_only_the_model() {
    assert_eq!(
        show_body("gpt-oss:120b").expect("the body encodes"),
        br#"{"model":"gpt-oss:120b"}"#
    );
}

#[test]
fn capabilities_and_the_architecture_context_window_decode() {
    let probe = show(
        r#"{"license":"x","capabilities":["completion","tools","thinking","vision"],
            "model_info":{"general.architecture":"gptoss","gptoss.context_length":131072,
                          "gptoss.block_count":24},"modified_at":"2026-01-01"}"#,
    );
    assert_eq!(
        probe.capabilities.as_deref(),
        Some(
            &[
                String::from("completion"),
                String::from("tools"),
                String::from("thinking"),
                String::from("vision"),
            ][..]
        )
    );
    assert_eq!(probe.context_window, Some(131_072));
    assert_eq!(probe.reports("vision"), Some(true));
    assert_eq!(probe.reports("embedding"), Some(false));
}

#[test]
fn the_architecture_key_outranks_an_earlier_context_key() {
    // A projector or vision tower can list its own context before the text
    // model's; the architecture-named member is the one that sizes the model.
    let probe = show(
        r#"{"model_info":{"clip.context_length":77,"general.architecture":"llama",
            "llama.context_length":8192}}"#,
    );
    assert_eq!(probe.context_window, Some(8_192));
}

#[test]
fn without_an_architecture_member_the_first_suffix_match_wins() {
    assert_eq!(
        show(r#"{"model_info":{"a.block_count":3,"glm.context_length":1000000}}"#).context_window,
        Some(1_000_000)
    );
    assert_eq!(
        show(r#"{"model_info":{"x.num_ctx":4096}}"#).context_window,
        Some(4_096)
    );
    assert_eq!(
        show(r#"{"model_info":{"x.context_window":2048}}"#).context_window,
        Some(2_048)
    );
    // The architecture names a key the body does not have: fall back.
    assert_eq!(
        show(r#"{"model_info":{"general.architecture":"qwen","llama.context_length":512}}"#)
            .context_window,
        Some(512)
    );
}

#[test]
fn only_a_positive_u32_counts_as_a_context_window() {
    for body in [
        r#"{"model_info":{"a.context_length":0}}"#,
        r#"{"model_info":{"a.context_length":-5}}"#,
        r#"{"model_info":{"a.context_length":"8192"}}"#,
        r#"{"model_info":{"a.context_length":4294967296}}"#,
        r#"{"model_info":{"a.context_length":null}}"#,
        r#"{"model_info":[1,2]}"#,
        r#"{"model_info":null}"#,
        r"{}",
    ] {
        assert_eq!(show(body).context_window, None, "{body}");
    }
}

#[test]
fn a_body_without_capabilities_leaves_every_capability_unknown() {
    for body in [
        r"{}",
        r#"{"capabilities":null}"#,
        r#"{"capabilities":"tools"}"#,
    ] {
        let probe = show(body);
        assert_eq!(probe.capabilities, None, "{body}");
        assert_eq!(probe.reports("tools"), None, "{body}");
    }
    // An empty list is a statement: the model has none of them.
    let probe = show(r#"{"capabilities":[]}"#);
    assert_eq!(probe.reports("tools"), Some(false));
    // Non-string members are dropped, strings kept.
    assert_eq!(
        show(r#"{"capabilities":["tools",7,null,"vision"]}"#).capabilities,
        Some(vec![String::from("tools"), String::from("vision")])
    );
}

#[test]
fn a_body_that_is_not_a_json_object_is_a_protocol_error() {
    for body in [&b"<html>no</html>"[..], b"", b"[]", b"7"] {
        assert!(
            matches!(
                decode_show(body),
                Err(ProviderError::Protocol {
                    family: Family::Ollama,
                    ..
                })
            ),
            "{}",
            String::from_utf8_lossy(body)
        );
    }
}

#[test]
fn ladders_follow_the_model_and_the_deployment() {
    let local = Deployment::SelfHosted;
    let cloud = Deployment::Cloud;
    assert_eq!(thinking_ladder(local, "qwen3:8b"), [Low, Medium, High, Max]);
    assert_eq!(thinking_ladder(local, "gpt-oss:20b"), [Low, Medium, High]);
    assert_eq!(thinking_ladder(cloud, "gpt-oss:120b"), [Low, Medium, High]);
    for id in [
        "deepseek-v4.1-flash",
        "deepseek-v4-flash:0731",
        "deepseek-v4-pro:0813",
        "deepseek-v4-flash",
    ] {
        assert_eq!(thinking_ladder(cloud, id), [Low, High, Max], "{id}");
        assert_eq!(thinking_ladder(local, id), [Low, High, Max], "{id}");
    }
    assert_eq!(thinking_ladder(cloud, "glm-5.2"), [High, Max]);
    assert_eq!(thinking_ladder(cloud, "glm-5.2:cloud"), [High, Max]);
    assert_eq!(thinking_ladder(cloud, "glm-5.3"), [Low, High, Max]);
    assert_eq!(
        thinking_ladder(cloud, "glm-5.1"),
        [Minimal, Low, Medium, High]
    );
    assert_eq!(
        thinking_ladder(cloud, "qwen3:32b"),
        [Minimal, Low, Medium, High]
    );
    // The cloud's GLM ladders do not leak to a self-hosted GLM.
    assert_eq!(thinking_ladder(local, "glm-5.2"), [Low, Medium, High, Max]);
}

#[test]
fn the_output_cap_names_exactly_the_two_deepseek_v4_deployments() {
    for id in [
        "deepseek-v4-flash",
        "deepseek-v4-pro",
        "deepseek-v4-flash:cloud",
        "DeepSeek-V4-Pro:0813",
    ] {
        assert!(cloud_output_capped(id), "{id}");
    }
    for id in [
        "deepseek-v4.1-flash",
        "deepseek-v3.1:671b",
        "gpt-oss:120b",
        "",
    ] {
        assert!(!cloud_output_capped(id), "{id}");
    }
}

#[test]
fn recorded_replies_report_their_exact_think_values() {
    let ladder = |body: &str| {
        show(body)
            .thinking
            .map(|values| values.ladder())
            .expect("the reply reports think values")
    };
    assert_eq!(ladder(GPT_OSS), (vec![Low, Medium, High], false));
    assert_eq!(ladder(GLM_5_2), (vec![High, Max], true));
    assert_eq!(ladder(DEEPSEEK_V41), (vec![Low, High, Max], true));
    assert_eq!(
        ladder(BEAM),
        (vec![Low, Medium, High, ThinkingLevel::Xhigh, Max], false)
    );
    // No named levels: reasoning is a switch, spelled by the boolean level.
    assert_eq!(ladder(GEMMA_4), (vec![BOOLEAN_ON], true));
    assert_eq!(ladder(LOCAL_QWEN3), (vec![BOOLEAN_ON], true));
    assert_eq!(ladder(NEMOTRON_ULTRA), (vec![BOOLEAN_ON], true));
    // `[true]`: always on, no way to turn it off.
    assert_eq!(ladder(MINIMAX_M2_7), (vec![BOOLEAN_ON], false));
    // A reasoning model whose server reports no values leaves the ladder to
    // the model id.
    let unreported = show(MISTRAL_LARGE_4);
    assert_eq!(unreported.reports("thinking"), Some(true));
    assert_eq!(unreported.thinking, None);
}

#[test]
fn recorded_replies_size_the_context_window_whatever_the_architecture_spells() {
    for (body, window) in [
        (GPT_OSS, 131_072),
        (GLM_5_2, 1_048_576),
        (MINIMAX_M2_7, 196_608),
        // An empty architecture still names its `.context_length` member.
        (NEMOTRON_ULTRA, 262_144),
        (LOCAL_QWEN3, 40_960),
        (LOCAL_GEMMA3, 32_768),
    ] {
        assert_eq!(show(body).context_window, Some(window), "{body}");
    }
    assert_eq!(show(LOCAL_GEMMA3).thinking, None);
    assert_eq!(show(LOCAL_GEMMA3).reports("thinking"), Some(false));
}

#[test]
fn a_thinking_object_without_usable_values_reports_nothing() {
    for body in [
        r#"{"thinking":{}}"#,
        r#"{"thinking":null}"#,
        r#"{"thinking":true}"#,
        r#"{"thinking":{"values":"high"}}"#,
    ] {
        assert_eq!(show(body).thinking, None, "{body}");
    }
    // Unknown strings are skipped, duplicates collapse, order is the server's.
    let probe = show(r#"{"thinking":{"values":["high","none","high","low","ultra",false]}}"#);
    let values = probe.thinking.expect("values report");
    assert_eq!(values.levels, [High, Low]);
    assert!(values.off && !values.on);
    // `values: []` is a report of nothing acceptable.
    let empty = show(r#"{"thinking":{"values":[]}}"#)
        .thinking
        .expect("values report");
    assert_eq!(empty.ladder(), (Vec::new(), false));
}

#[test]
fn a_context_window_is_accepted_from_one_up_to_u32_max() {
    // Both ends of the accepted range, and the values a signed 32-bit
    // reading would wrongly refuse.
    for (tokens, expected) in [
        ("1", 1_u32),
        ("2147483647", 2_147_483_647),
        ("2147483648", 2_147_483_648),
        ("4294967295", u32::MAX),
    ] {
        let body = format!(r#"{{"model_info":{{"a.context_length":{tokens}}}}}"#);
        assert_eq!(show(&body).context_window, Some(expected), "{tokens}");
    }
}

#[test]
fn named_levels_outrank_the_boolean_spellings_in_the_ladder() {
    let ladder = |values: &str| {
        show(&format!(r#"{{"thinking":{{"values":{values}}}}}"#))
            .thinking
            .expect("the reply reports think values")
            .ladder()
    };
    // `true` only matters when no named level is offered.
    assert_eq!(ladder(r#"["low",true]"#), (vec![Low], false));
    assert_eq!(ladder(r#"[true,"high",false]"#), (vec![High], true));
    assert_eq!(ladder("[true]"), (vec![BOOLEAN_ON], false));
    assert_eq!(ladder("[false]"), (Vec::new(), true));
    assert_eq!(
        ladder(r#"["xhigh","max"]"#),
        (vec![ThinkingLevel::Xhigh, Max], false)
    );
}
