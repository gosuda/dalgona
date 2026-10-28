//! Def-substitution authority probe (F-1): a caller-cloned entry whose
//! authority-bearing static `def` is replaced must be refused before any
//! credential use, even when every compared connection field still matches.
//!
//! The attack configures the real `anthropic` default slot and the real `bai`
//! row through the strict host path (`Config::load` +
//! `ProviderConfig::from_config`, so `origins` stay private default-origin
//! data), clones the `anthropic` entry, and substitutes only the `bai` def.
//! The bound model is a real BAI Anthropic id (`claude-sonnet-5`), whose
//! static wire has family `Anthropic`, so the current wire-authorization gate
//! accepts while the endpoint would leave the slot's configured default.
//!
//! No request is ever opened and no key is ever sent: the test asserts only
//! on the `Provider::new` constructor result, using a captured fake
//! environment and a temporary private store. Predicted state before the root
//! fix: `Provider::new` returns `Ok` (ACCEPTANCE), so the expected typed
//! refusal fails RED. After the root fix (authenticate the full routing
//! authority, including `def`, against the set-owned slot) the same call
//! refuses typed with the existing slot-mismatch contract.

use super::*;

#[test]
fn caller_cloned_entry_with_substituted_def_is_refused_before_any_request() -> TestResult {
    let dir = TestDir::new()?;
    let config = Config::load(
        ConfigProduct::Dalgon,
        dir.path(),
        "",
        Some("[providers.anthropic]\n[providers.bai]\n"),
    )?;
    let providers = ProviderConfig::from_config(&config)?;

    let anthropic_entry = providers
        .providers
        .iter()
        .find(|entry| &*entry.id == "anthropic")
        .ok_or_else(|| io::Error::other("the anthropic default slot is configured"))?
        .clone();
    assert_eq!(anthropic_entry.family, Family::Anthropic);
    let bai_entry = providers
        .providers
        .iter()
        .find(|entry| &*entry.id == "bai")
        .ok_or_else(|| io::Error::other("the bai row is configured"))?;
    let bai_def = bai_entry
        .def
        .ok_or_else(|| io::Error::other("the bai row carries a static definition"))?;
    assert!(
        bai_def.wires.iter().any(|wire| wire.select
            == crate::provider::wire::WireSelect::Exact("claude-sonnet-5")
            && wire.family == Family::Anthropic),
        "the probe model stays bound by the real bai static row"
    );

    let set = ProviderSet::new(
        &providers,
        identity(),
        EnvSnapshot::test(&[
            ("ANTHROPIC_API_KEY", "anthropic-authority-probe-fake-key"),
            ("BAI_API_KEY", "bai-authority-probe-fake-key"),
        ]),
        dir.path(),
        dir.path(),
    )?;

    // Control: the authored anthropic default binds through the safe path,
    // so a later refusal cannot be mistaken for an unrelated catalog or
    // credential failure.
    let mut control_route = resolved("anthropic", Family::Anthropic, "control-model");
    control_route.entry.thinking = ThinkingSupport::UnknownAdaptive;
    let _control = set.provider(control_route).map_err(|error| {
        io::Error::other(format!("the authored anthropic default binds: {error}"))
    })?;

    // The external-like forgery: only `def` differs from the owned slot.
    let slot = set
        .inner
        .providers
        .get("anthropic")
        .ok_or_else(|| io::Error::other("the anthropic slot is owned by the set"))?;
    assert_eq!(anthropic_entry.id, slot.entry.id);
    assert_eq!(anthropic_entry.family, slot.entry.family);
    assert_eq!(&*anthropic_entry.base_url, &*slot.entry.base_url);
    assert_eq!(anthropic_entry.transport, slot.entry.transport);
    assert_eq!(anthropic_entry.key_env, slot.entry.key_env);
    assert_eq!(anthropic_entry.auth, slot.entry.auth);
    assert_eq!(
        anthropic_entry.max_concurrent_requests,
        slot.entry.max_concurrent_requests
    );
    assert_eq!(anthropic_entry.settings, slot.entry.settings);
    let mut forged = anthropic_entry.clone();
    forged.def = Some(bai_def);

    // Opaque slot credential through the public seam; never handled as text
    // and never sent anywhere by this test.
    let credential = set.credential("anthropic")?;
    let client = slot.client.get().clone();

    let mut attack_route = resolved("anthropic", Family::Anthropic, "claude-sonnet-5");
    attack_route.entry.thinking = ThinkingSupport::UnknownAdaptive;
    let Err(error) = Provider::new(attack_route, forged, credential, &client, &set) else {
        panic!(
            "ACCEPTANCE (predicted RED): the cloned entry with the substituted \
             bai definition bound its Anthropic wire instead of refusing"
        )
    };
    match &error {
        ProviderError::InvalidRequest { message }
            if message.as_str() == "provider anthropic does not match this provider set" => {}
        _ => panic!("expected the typed slot-ownership refusal, got a different result"),
    }
    Ok(())
}
