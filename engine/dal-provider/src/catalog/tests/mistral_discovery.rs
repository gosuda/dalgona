//! Mistral card discovery: `capabilities.reasoning` decides whether a row
//! sends `reasoning_effort` at all, and which values it may carry.

use dal_core::ThinkingLevel::{High, Low, Max};

use super::*;
use crate::catalog::resolve::default_entry;

fn mistral() -> ProviderEntry {
    provider("mistral", Family::Mistral, "https://api.mistral.ai/v1")
}

fn ladder(entry: &CatalogEntry) -> (Vec<dal_core::ThinkingLevel>, bool) {
    match &entry.thinking {
        ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        } => (accepted.clone(), *none_supported),
        other => panic!("{} carries {other:?}", entry.id),
    }
}

fn card(id: &str, capabilities: &str) -> CatalogEntry {
    let body = format!(
        r#"{{"data":[{{"id":"{id}","capabilities":{{{capabilities}}},"max_context_length":131072}}]}}"#
    );
    let mut rows = decode_mistral_models(&mistral(), body.as_bytes()).expect("a card decodes");
    assert_eq!(rows.len(), 1);
    rows.remove(0)
}

#[test]
fn an_adjustable_reasoning_card_takes_none_and_high() {
    let entry = card(
        "mistral-small-latest",
        r#""completion_chat":true,"function_calling":true,"reasoning":true"#,
    );
    assert_eq!(ladder(&entry), (vec![High], true));
}

#[test]
fn a_card_that_does_not_reason_sends_no_reasoning_control() {
    let entry = card(
        "mistral-large-2411",
        r#""completion_chat":true,"reasoning":false"#,
    );
    assert_eq!(ladder(&entry), (Vec::new(), false));
}

#[test]
fn a_card_that_names_no_reasoning_flag_keeps_the_typed_default() {
    let entry = card("open-mistral-7b", r#""completion_chat":true"#);
    assert_eq!(ladder(&entry), (Vec::new(), false));
}

#[test]
fn the_hosted_glm_cards_name_their_own_ladders() {
    let glm_5_2 = card("zai-glm-5-2", r#""completion_chat":true,"reasoning":true"#);
    assert_eq!(ladder(&glm_5_2), (vec![High, Max], true));
    // `zai-glm-5-3` has no `none`: thinking cannot be turned off.
    let glm_5_3 = card("zai-glm-5-3", r#""completion_chat":true,"reasoning":true"#);
    assert_eq!(ladder(&glm_5_3), (vec![Low, High, Max], false));
}

#[test]
fn a_hosted_glm_card_that_does_not_reason_has_no_ladder() {
    // The card's flag decides before the id does: `reasoning: false` on a glm
    // id must not borrow the glm ladder.
    for id in ["zai-glm-5-2", "zai-glm-5-3"] {
        let entry = card(id, r#""completion_chat":true,"reasoning":false"#);
        assert_eq!(ladder(&entry), (Vec::new(), false), "{id}");
    }
}

#[test]
fn a_typed_mistral_id_sends_no_reasoning_control_while_other_families_keep_none() {
    // `reasoning_effort: none` is only valid on a model that reasons, and a
    // typed id has no card to say so; the Ollama `think: false` spelling is
    // harmless on every model and keeps its default.
    assert_eq!(
        ladder(&default_entry(&mistral(), "typed-model")),
        (Vec::new(), false)
    );
    assert_eq!(
        ladder(&default_entry(
            &provider("ollama", Family::Ollama, "http://127.0.0.1:11434"),
            "typed-model"
        )),
        (Vec::new(), true)
    );
}
