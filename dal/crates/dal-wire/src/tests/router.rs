#[path = "router_host.rs"]
mod host;
#[path = "router_models.rs"]
mod models;
#[path = "router_relay.rs"]
mod relay;

use proptest::prelude::*;
use sonic_rs::Value;

use crate::router::{
    DigestTable, Route, RouteMatch, canonical_json, digest_history, ignored_members, match_route,
};

#[test]
fn unknown_routes_and_wrong_methods_have_exact_shapes() {
    assert!(matches!(
        match_route("GET", "/v1/nope"),
        RouteMatch::NotFound { method, path }
        if method == "GET" && path == "/v1/nope"
    ));
    assert!(matches!(
        match_route("GET", "/v1/chat/completions"),
        RouteMatch::MethodNotAllowed { allow, .. } if allow == "POST"
    ));
    assert_eq!(
        match_route("POST", "/v1/chat/completions?stream=true"),
        RouteMatch::Found(Route::Chat)
    );
}

#[test]
fn unknown_members_are_sorted_for_the_ignored_header() {
    let request = sonic_rs::from_str::<Value>(
        r#"{"model":"dalgon/normal","temperature":0.5,"zebra":1,"apple":2}"#,
    )
    .expect("request is JSON");
    assert_eq!(
        ignored_members(&request, &["model"]),
        ["apple", "temperature", "zebra"]
    );
}

proptest! {
    #[test]
    fn shuffled_keys_and_whitespace_preserve_the_digest(
        first in "[a-z]{1,8}",
        second in "[a-z]{1,8}",
        number in any::<i64>(),
    ) {
        prop_assume!(first != second);
        let ordered = sonic_rs::from_str::<Value>(&format!(
            r#"{{"{first}":{number},"nested":{{"b":2,"a":1}},"second":"{second}"}}"#,
        ))
        .expect("ordered history is JSON");
        let shuffled = sonic_rs::from_str::<Value>(&format!(
            r#"{{ "second" : "{second}" , "nested" : {{ "b" : 2 , "a" : 1 }} , "{first}" : {number} }}"#,
        ))
        .expect("shuffled history is JSON");
        prop_assert_eq!(canonical_json(&ordered), canonical_json(&shuffled));
        prop_assert_eq!(digest_history(&[ordered]), digest_history(&[shuffled]));
    }
}

#[test]
fn digest_table_evicts_the_oldest_entry() {
    let mut table = DigestTable::new(2);
    table.insert("a".to_owned(), "s1".to_owned());
    table.insert("b".to_owned(), "s2".to_owned());
    table.insert("c".to_owned(), "s3".to_owned());
    assert_eq!(table.get("a"), None);
    assert_eq!(table.get("b"), Some("s2"));
    assert_eq!(table.get("c"), Some("s3"));
}

#[test]
fn digest_table_refreshes_recency_on_read() {
    let mut table = DigestTable::new(2);
    table.insert("a".to_owned(), "s1".to_owned());
    table.insert("b".to_owned(), "s2".to_owned());
    assert_eq!(table.get("a"), Some("s1"));
    table.insert("c".to_owned(), "s3".to_owned());
    assert_eq!(table.get("a"), Some("s1"));
    assert_eq!(table.get("b"), None);
    assert_eq!(table.get("c"), Some("s3"));
}
