//! Integration coverage for the host documentation seam.
#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
use std::collections::BTreeMap;
use std::sync::Arc;

use dal_agent::error::SchemeError;
use dal_agent::ext::{Doc, Extension, ExtensionBuilder};
use dal_agent::{Env, Host, HostError, Product};
use dal_core::{Config, ConfigProduct, ServiceSet};

struct TestScheme;

impl dal_agent::ext::SchemeResolver for TestScheme {
    fn read<'a>(
        &'a self,
        path: &'a str,
        _cx: &'a dal_agent::ext::SchemeCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, Result<dal_agent::ext::Doc, dal_agent::error::SchemeError>>
    {
        Box::pin(async move { Ok(dal_agent::ext::Doc::new(format!("test://{path}"), "body")) })
    }
}

/// A generation-static resolver: `dyn` answers with its own text, `refuse`
/// answers with a definitive failure, every other path defers to the page
/// table by returning `None`.
struct StaticScheme;

impl dal_agent::ext::SchemeResolver for StaticScheme {
    fn read<'a>(
        &'a self,
        path: &'a str,
        _cx: &'a dal_agent::ext::SchemeCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, Result<Doc, SchemeError>> {
        Box::pin(async move {
            Err(SchemeError::NotFound {
                uri: format!("live://{path}").into(),
            })
        })
    }

    fn read_static(&self, path: &str) -> Option<Result<Doc, SchemeError>> {
        match path {
            "dyn" => Some(Ok(Doc::new("live://dyn", "from the resolver"))),
            "refuse" => Some(Err(SchemeError::Failed {
                message: "the resolver refused this page".into(),
            })),
            _ => None,
        }
    }
}

async fn start_with(
    tmp: &tempfile::TempDir,
    extensions: Vec<Extension>,
) -> Result<Host, HostError> {
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    let config = Config::load(ConfigProduct::Dalgon, &data, "", None).expect("config");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: data,
        sandbox_helper: None,
    };
    Host::start(product, config, env).await
}

async fn start(tmp: &tempfile::TempDir, extensions: Vec<Extension>) -> Host {
    start_with(tmp, extensions).await.expect("host")
}

fn manual() -> Extension {
    ExtensionBuilder::new("manual", "0.1.0", ServiceSet::default())
        .expect("builder")
        .doc("guide/usage", "Usage", "usage text")
        .doc("alpha-two", "Alpha two", "second alpha")
        .doc("guide/setup", "Setup", "setup text")
        .doc("alpha-one", "Alpha one", "first alpha")
        .build()
        .expect("manual extension")
}

#[tokio::test]
async fn docs_list_and_page_read_roundtrip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).expect("data dir");
    let config = Config::load(ConfigProduct::Dalgon, &data, "", None).expect("config");
    let extension =
        dal_agent::ext::ExtensionBuilder::new("test", "0.1.0", dal_core::ServiceSet::default())
            .expect("builder")
            .scheme("test", std::sync::Arc::new(TestScheme))
            .doc("protocol", "Protocol", "body")
            .build()
            .expect("extension");
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: data.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let entries = host.docs();
    assert_eq!(entries.len(), 1, "one record listed");
    assert_eq!(entries[0].uri.as_ref(), "test://protocol");
    assert_eq!(entries[0].title.as_ref(), "Protocol");
    let doc = host.doc("test://protocol").expect("page reads back");
    assert_eq!(doc.text.as_ref(), "body");
}

/// Pages registered out of order across two extensions list in URI order,
/// each row keeps its own title, and each URI reads back its own text.
#[tokio::test]
async fn docs_list_in_uri_order_and_each_row_reads_its_own_page() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let zeta = ExtensionBuilder::new("zeta", "0.1.0", ServiceSet::default())
        .expect("builder")
        .doc("b", "Bee", "bee text")
        .doc("a", "Ay", "ay text")
        .build()
        .expect("zeta extension");
    let host = start(&tmp, vec![zeta, manual()]).await;
    let listed: Vec<(String, String)> = host
        .docs()
        .iter()
        .map(|entry| (entry.uri.to_string(), entry.title.to_string()))
        .collect();
    let expected: Vec<(String, String)> = [
        ("manual://alpha-one", "Alpha one"),
        ("manual://alpha-two", "Alpha two"),
        ("manual://guide/setup", "Setup"),
        ("manual://guide/usage", "Usage"),
        ("zeta://a", "Ay"),
        ("zeta://b", "Bee"),
    ]
    .iter()
    .map(|(uri, title)| ((*uri).to_owned(), (*title).to_owned()))
    .collect();
    assert_eq!(listed, expected, "rows list in URI order with their titles");
    for (uri, text) in [
        ("manual://alpha-one", "first alpha"),
        ("manual://alpha-two", "second alpha"),
        ("manual://guide/setup", "setup text"),
        ("manual://guide/usage", "usage text"),
        ("zeta://a", "ay text"),
        ("zeta://b", "bee text"),
    ] {
        let doc = host.doc(uri).expect("listed page reads back");
        assert_eq!(doc.uri.as_ref(), uri, "the read echoes its request");
        assert_eq!(doc.text.as_ref(), text, "{uri} reads its own text");
    }
}

/// A miss within two edits of a published URI names the nearest page; the
/// boundary is distance 2 (hit) against distance 3 (plain miss), a tie
/// goes to the first URI in order, and a strictly closer later page wins.
#[tokio::test]
async fn a_near_miss_names_its_nearest_page_within_two_edits() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let host = start(&tmp, vec![manual()]).await;
    let near = |query: &str| match host.doc(query) {
        Err(SchemeError::Near { uri, nearest }) => {
            assert_eq!(uri.as_ref(), query, "the miss echoes the full query");
            Some(nearest.to_string())
        }
        Err(SchemeError::NotFound { uri }) => {
            assert_eq!(uri.as_ref(), query, "the miss echoes the full query");
            None
        }
        other => panic!("{query} must miss typed, got {other:?}"),
    };
    assert_eq!(
        near("manual://alpha-on").as_deref(),
        Some("manual://alpha-one"),
        "one deletion away"
    );
    assert_eq!(
        near("manual://alpha-o").as_deref(),
        Some("manual://alpha-one"),
        "two edits from both alpha pages: the tie goes to the first URI in order"
    );
    assert_eq!(
        near("manual://alpha-twe").as_deref(),
        Some("manual://alpha-two"),
        "a strictly closer later page beats an earlier page at distance 2"
    );
    assert_eq!(
        near("manual://alpha-"),
        None,
        "three edits from every page is a plain miss"
    );
    assert_eq!(
        near("manual://guide/setu").as_deref(),
        Some("manual://guide/setup"),
        "the suggestion follows the page grammar's nested paths"
    );
    assert_eq!(
        near("manual://alpha-one/").as_deref(),
        Some("manual://alpha-one"),
        "a trailing slash is a near miss, never a hit"
    );
    assert_eq!(
        near("manual://Alpha-one").as_deref(),
        Some("manual://alpha-one"),
        "path case is significant: the upper-case spelling is a near miss"
    );
    assert_eq!(
        near("MANUAL://alpha-one"),
        None,
        "scheme case is significant: six substitutions away is a plain miss"
    );
}

/// Hostile and malformed URIs fail typed: no panic, no page, and the
/// resolver-less shapes (`no ://`, empty scheme) are unknown schemes.
#[tokio::test]
async fn malformed_uris_fail_typed_and_never_resolve_a_page() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let host = start(&tmp, vec![manual()]).await;
    for query in [
        "",
        "manual",
        "manual:/alpha-one",
        "://alpha-one",
        "manual//x",
    ] {
        match host.doc(query) {
            Err(SchemeError::Unknown { .. }) => {}
            other => panic!("{query:?} has no scheme separator or scheme, got {other:?}"),
        }
    }
    match host.doc("manual://") {
        Err(SchemeError::NotFound { uri }) => assert_eq!(uri.as_ref(), "manual://"),
        other => panic!("an empty page path is a plain miss, got {other:?}"),
    }
    let hostile = format!("manual://{}", "a".repeat(100_000));
    match host.doc(&hostile) {
        Err(SchemeError::NotFound { uri }) => {
            assert_eq!(uri.len(), hostile.len(), "the full hostile URI is echoed");
        }
        other => panic!("a 100 KB path is a plain miss, got {other:?}"),
    }
    let multibyte = "manual://alpha-on\u{e9}";
    match host.doc(multibyte) {
        Err(SchemeError::Near { nearest, .. }) => {
            assert_eq!(nearest.as_ref(), "manual://alpha-one");
        }
        other => panic!("a two-byte character is still within two edits, got {other:?}"),
    }
    // A scheme nobody registered must not resolve; whether it reports as an
    // unknown scheme or a plain miss is not pinned here.
    assert!(
        matches!(
            host.doc("nosuch://alpha-one"),
            Err(SchemeError::Unknown { .. } | SchemeError::NotFound { .. })
        ),
        "an unregistered scheme resolves nothing"
    );
}

/// A scheme's static resolver answers first, including its refusals; a
/// resolver that defers with `None` falls through to the page table.
#[tokio::test]
async fn a_static_resolver_verdict_beats_the_page_table_and_none_falls_through() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let live = ExtensionBuilder::new("live", "0.1.0", ServiceSet::default())
        .expect("builder")
        .scheme("live", Arc::new(StaticScheme))
        .doc("dyn", "Dyn", "from the page")
        .doc("refuse", "Refuse", "from the page")
        .doc("page", "Page", "from the page")
        .build()
        .expect("live extension");
    let host = start(&tmp, vec![live]).await;
    let resolved = host.doc("live://dyn").expect("resolver answers");
    assert_eq!(
        resolved.text.as_ref(),
        "from the resolver",
        "the resolver's page shadows the same-URI table page"
    );
    match host.doc("live://refuse") {
        Err(SchemeError::Failed { message }) => {
            assert_eq!(message.as_ref(), "the resolver refused this page");
        }
        other => panic!("a resolver refusal must stand over the table page, got {other:?}"),
    }
    let fallthrough = host.doc("live://page").expect("deferred page reads");
    assert_eq!(
        fallthrough.text.as_ref(),
        "from the page",
        "a resolver returning None defers to the page table"
    );
}

/// Page text is served verbatim: empty, whitespace-padded, multi-byte and
/// megabyte-sized bodies come back byte for byte.
#[tokio::test]
async fn page_text_is_served_byte_for_byte() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let padded = "  \n\tleading and trailing space\n\n";
    let multibyte = "caf\u{e9} \u{65e5}\u{672c}\u{8a9e} \u{1f30d} end";
    let big = format!("{}\u{e9}", "x".repeat(1_048_576));
    let extension = ExtensionBuilder::new("body", "0.1.0", ServiceSet::default())
        .expect("builder")
        .doc("empty", "Empty", "")
        .doc("padded", "Padded", padded)
        .doc("multibyte", "Multibyte", multibyte)
        .doc("big", "Big", &big)
        .build()
        .expect("body extension");
    let host = start(&tmp, vec![extension]).await;
    for (uri, text) in [
        ("body://empty", ""),
        ("body://padded", padded),
        ("body://multibyte", multibyte),
        ("body://big", big.as_str()),
    ] {
        let doc = host.doc(uri).expect("page reads");
        assert_eq!(doc.text.len(), text.len(), "{uri} keeps its byte length");
        assert_eq!(doc.text.as_ref(), text, "{uri} reads back byte for byte");
    }
}

/// Two pages claiming one URI refuse the whole host start with the
/// registration text, so no host serves an ambiguous page.
#[tokio::test]
async fn a_duplicate_doc_uri_refuses_the_host_start() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let extension = ExtensionBuilder::new("manual", "0.1.0", ServiceSet::default())
        .expect("builder")
        .doc("index", "Index", "first")
        .doc("index", "Index", "second")
        .build()
        .expect("each page is valid alone");
    match start_with(&tmp, vec![extension]).await {
        Err(HostError::Config { message }) => assert_eq!(
            message.as_ref(),
            "doc uri 'manual://index' is already registered"
        ),
        Err(other) => panic!("a duplicate doc URI is a config refusal, got {other:?}"),
        Ok(_) => panic!("a duplicate doc URI must not start a host"),
    }
}
