use super::StateKey;

#[test]
fn r08_state_keys_are_identifiers_not_paths() {
    let longest = format!("a{}", "z".repeat(63));
    for valid in ["a", "cache.v2", "a_b-c.d", longest.as_str()] {
        assert_eq!(
            StateKey::parse(valid).map(|key| key.to_string()),
            Ok(valid.to_owned())
        );
    }
    let too_long = format!("a{}", "z".repeat(64));
    for invalid in [
        "",
        "1a",
        ".a",
        "A",
        "a/b",
        "../a",
        "a b",
        "é",
        too_long.as_str(),
    ] {
        assert!(
            StateKey::parse(invalid).is_err(),
            "{invalid:?} was accepted"
        );
    }
}
