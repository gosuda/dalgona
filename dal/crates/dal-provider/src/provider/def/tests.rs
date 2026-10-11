use super::*;

#[test]
fn ids_are_unique_and_match_the_grammar() {
    for (index, def) in PROVIDERS.iter().enumerate() {
        assert!(
            !def.id.is_empty()
                && def
                    .id
                    .bytes()
                    .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-')),
            "{} is not [a-z0-9-]+",
            def.id
        );
        assert!(
            PROVIDERS[..index]
                .iter()
                .all(|earlier| earlier.id != def.id),
            "{} appears twice",
            def.id
        );
    }
}

#[test]
fn rows_that_share_a_loopback_share_a_client() {
    for (index, first) in PROVIDERS.iter().enumerate() {
        let Some(first_oauth) = first.oauth else {
            continue;
        };
        for second in &PROVIDERS[index + 1..] {
            let Some(second_oauth) = second.oauth else {
                continue;
            };
            let same_loopback = first_oauth.redirect.host == second_oauth.redirect.host
                && first_oauth.redirect.port == second_oauth.redirect.port;
            assert!(
                !same_loopback || first_oauth.client_id == second_oauth.client_id,
                "{} and {} share a loopback but not a client",
                first.id,
                second.id
            );
        }
    }
}

#[test]
fn every_row_signs_in_somehow() {
    for def in PROVIDERS {
        assert!(
            def.key.is_some() || def.oauth.is_some(),
            "{} has neither a key nor an OAuth sign-in",
            def.id
        );
        assert!(
            def.methods().next().is_some(),
            "{} offers no method",
            def.id
        );
    }
}

#[test]
fn methods_follow_the_row() {
    let methods = |id: &str| find(id).map(|def| def.methods().collect::<Vec<_>>());
    assert_eq!(methods("openai"), Some(vec![Method::ApiKey]));
    assert_eq!(
        methods("anthropic"),
        Some(vec![Method::ApiKey, Method::Browser])
    );
    assert_eq!(
        methods("openai-codex"),
        Some(vec![Method::Browser, Method::Device])
    );
    assert_eq!(methods("nobody"), None);
}
