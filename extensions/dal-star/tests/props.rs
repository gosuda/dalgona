//! Deterministic table equivalents for the adapter property suite.
//!
//! Each test states its oracle in plain words and checks it without citing
//! any planning material. Table-driven loops stand in where a property
//! framework would sample: every case is fixed, every oracle is an
//! independent recomputation, and every assertion names the behavior that
//! must hold.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests use unwrap/expect/panic freely per repo test convention"
)]
#[path = "support/host.rs"]
pub mod host;
pub mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dal_agent::ext::tool::{ToolCall, ToolOutcome};
use dal_agent::{GrantKey, GrantStore};
use dal_core::ext::OpSet;
use dal_core::{ClientId, Name, Origin, Part, PluginLimits, RawJson, ServiceSet, Timestamp};
use dal_star::{CELL_LIMITS, LoadRoots, PluginsConfig, eval_extension, load};
use host::{RecordingHost, eval_args, tool_cx_approved};
use sonic_rs::JsonValueMutTrait as _;

fn plugins_config_with(enabled: &[&str]) -> PluginsConfig {
    PluginsConfig {
        enabled: enabled.iter().map(|name| (*name).to_owned()).collect(),
        limits: PluginLimits::default(),
        configs: BTreeMap::new(),
    }
}

fn minimal_plugin(name: &str) -> String {
    format!(
        "load(\"@dal/v1\", \"dal\")\ndef run(ctx, args):\n    return \"{name}-ok\"\ntool = dal.tool(description = \"d\", input = dal.schema(), run = run)\nplugin = dal.plugin(name = \"{name}\", version = \"0.1.0\", tools = {{\"t\": tool}})\n"
    )
}

fn write_plugins(data: &std::path::Path, names: &[&str]) {
    for name in names {
        let dir = data.join("plugins").join(name);
        std::fs::create_dir_all(&dir).expect("plugin dir");
        std::fs::write(dir.join("plugin.star"), minimal_plugin(name)).expect("plugin source");
    }
}

/// Dispatch order equals the canonical sorted oracle for permuted inputs.
///
/// Breaks if loading stops sorting by origin rank then name, or if the
/// enabled filter stops preserving the full generation order: any
/// permutation of the request list must still publish plugins sorted.
#[test]
fn prop_dispatch_order_matches_oracle() {
    let sets: &[&[&str]] = &[
        &["zeta", "alpha", "mid"],
        &["mid", "zeta", "alpha"],
        &["b", "a"],
        &["solo"],
        &["delta", "charlie", "bravo", "alpha"],
    ];
    for names in sets {
        let data = tempfile::tempdir().expect("data root");
        write_plugins(data.path(), names);
        // Permuted request orders over the same installed set.
        let forward: Vec<&str> = names.to_vec();
        let mut backward: Vec<&str> = names.to_vec();
        backward.reverse();
        for enabled in [forward, backward] {
            let roots = LoadRoots {
                data_root: data.path().to_path_buf(),
                bundled: Vec::new(),
            };
            let generation = load(&roots, &plugins_config_with(&enabled)).expect("load");
            let mut oracle: Vec<&str> = names.to_vec();
            oracle.sort_unstable();
            oracle.dedup();
            let actual: Vec<&str> = generation.names().collect();
            assert_eq!(
                actual, oracle,
                "permuted request {enabled:?} must publish sorted"
            );
            // Independent oracle: a set-sorted recomputation, not the loader.
            let set_oracle: Vec<String> = {
                let mut set = BTreeSet::new();
                for name in *names {
                    set.insert((*name).to_owned());
                }
                set.into_iter().collect()
            };
            let actual_owned: Vec<String> = actual.iter().map(|name| (*name).to_owned()).collect();
            assert_eq!(
                actual_owned, set_oracle,
                "order must match set-sorted oracle"
            );
        }
    }
}

async fn run_cell_text(code: &str) -> String {
    let extension = eval_extension(CELL_LIMITS).expect("eval extension");
    let tool = extension
        .tools()
        .iter()
        .find(|(tool, _)| tool.name().as_str() == "eval")
        .map(|(tool, _)| Arc::clone(tool))
        .expect("eval tool registered");
    let extensions = [extension];
    let host = RecordingHost::new(&extensions, OpSet::EMPTY, []);
    let args = RawJson::parse(&eval_args(code)).expect("cell args");
    match tool
        .run(
            ToolCall::new("test-call", args),
            tool_cx_approved(host.script_cx()),
        )
        .await
    {
        ToolOutcome::Ok(output) => match &output.parts[0] {
            Part::Text { text } => text.to_string(),
            other => panic!("expected text part, got {other:?}"),
        },
        other => panic!("expected completed cell, got {other:?}"),
    }
}

/// The same cell program produces identical results across reruns.
///
/// Breaks if evaluation shares mutable heaps across cells, leaks prints, or
/// admits nondeterministic builtins: two fresh evaluations of each fixture
/// must agree byte for byte while staying under the cell budget.
#[tokio::test(flavor = "multi_thread")]
async fn prop_cell_determinism() {
    let fixtures = [
        "1 + 2",
        "sorted([3, 1, 2])",
        "{\"b\": 1, \"a\": 2}",
        "[x * 2 for x in [1, 2, 3]]",
        "\"ab\" * 3",
        "print(\"hi\")\n41 + 1",
    ];
    for code in fixtures {
        let first = run_cell_text(code).await;
        let second = run_cell_text(code).await;
        assert!(
            first.contains("\"status\":\"completed\""),
            "{code}: {first}"
        );
        let strip_invocation = |text: &str| {
            let mut value: sonic_rs::Value = sonic_rs::from_str(text).expect("cell output is json");
            value
                .as_object_mut()
                .expect("cell output object")
                .remove(&"invocation_id");
            value
        };
        assert_eq!(
            strip_invocation(&first),
            strip_invocation(&second),
            "cell must rerun identically apart from the per-invocation id: {code}"
        );
    }
}

fn grant_key(
    ext: &str,
    origin: Origin,
    services: &[&str],
    workspace: &std::path::Path,
) -> GrantKey {
    GrantKey {
        extension: ext.parse::<Name>().expect("valid extension name"),
        origin,
        services: ServiceSet::from_names(services.iter().copied()).expect("valid services"),
        digest: "a".repeat(64).into_boxed_str(),
        workspace: workspace.to_path_buf(),
    }
}

/// Grant store operations mirror a plain map oracle.
///
/// Breaks if granting stops being idempotent, revocation stops removing
/// every key of an extension, or a changed capability set stops counting as
/// a new key: after every step the persisted list must equal the oracle set.
#[tokio::test(flavor = "multi_thread")]
async fn prop_grant_store_matches_map_oracle() {
    let dir = tempfile::tempdir().expect("data root");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let store = GrantStore::new(dir.path().to_path_buf());
    let by = ClientId::new("tui");
    let mut oracle = BTreeSet::new();

    // Approve two distinct keys.
    let key_a = grant_key("focus", Origin::User, &["fs.read", "run"], &workspace);
    let key_b = grant_key("focus", Origin::User, &["net"], &workspace);
    assert!(
        store
            .grant(key_a.clone(), by.clone(), Timestamp::now())
            .await
            .expect("grant a")
    );
    oracle.insert(key_a.clone());
    assert!(
        store
            .grant(key_b.clone(), by.clone(), Timestamp::now())
            .await
            .expect("grant b")
    );
    oracle.insert(key_b.clone());

    // Duplicate approve is idempotent and rewrites nothing.
    assert!(
        !store
            .grant(key_a.clone(), by.clone(), Timestamp::now())
            .await
            .expect("re-grant")
    );
    let listed: BTreeSet<GrantKey> = store
        .list()
        .await
        .expect("list")
        .into_iter()
        .map(|row| row.key)
        .collect();
    assert_eq!(listed, oracle, "duplicate grant must not change the set");
    assert!(store.contains(&key_a).await.expect("contains a"));
    assert!(store.contains(&key_b).await.expect("contains b"));

    // A changed capability set is a new key and starts absent.
    let changed = grant_key("focus", Origin::User, &["fs.read", "net"], &workspace);
    assert!(!oracle.contains(&changed));
    assert!(!store.contains(&changed).await.expect("changed absent"));
    assert!(
        store
            .grant(changed.clone(), by.clone(), Timestamp::now())
            .await
            .expect("grant changed")
    );
    oracle.insert(changed.clone());
    assert!(store.contains(&changed).await.expect("changed present"));

    // A different origin is a different key.
    let bundled = grant_key("focus", Origin::Bundled, &["fs.read", "run"], &workspace);
    assert!(!store.contains(&bundled).await.expect("bundled absent"));

    // Revoke removes every key of the extension and reports the count.
    let removed = store
        .revoke(&"focus".parse::<Name>().expect("name"))
        .await
        .expect("revoke");
    assert_eq!(removed, oracle.len(), "revoke must report every key");
    oracle.clear();
    let listed: BTreeSet<GrantKey> = store
        .list()
        .await
        .expect("list after revoke")
        .into_iter()
        .map(|row| row.key)
        .collect();
    assert_eq!(listed, oracle, "store must be empty after revoke");
    assert!(!store.contains(&key_a).await.expect("a gone"));

    // Revoking an absent extension is idempotent with zero rows.
    let removed = store
        .revoke(&"focus".parse::<Name>().expect("name"))
        .await
        .expect("second revoke");
    assert_eq!(removed, 0);
}

fn valid_generated_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..].iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
    })
}

/// Generated registration names stay unique over a bounded set.
///
/// Breaks if hook synthesis drops its per-event sequence number or widens
/// the alphabet: every generated hook identity must pass an independent
/// grammar check and land exactly once in the registration dump.
#[test]
fn prop_generated_names_unique() {
    let count = 16usize;
    let mut hooks = String::new();
    for _ in 0..count {
        hooks.push_str("dal.on(\"tool_call\", h), ");
    }
    let source = format!(
        "load(\"@dal/v1\", \"dal\")\ndef h(ctx, event):\n    return None\ndef run(ctx, args):\n    return \"ok\"\ntool = dal.tool(description = \"d\", input = dal.schema(), run = run)\nplugin = dal.plugin(name = \"probe\", version = \"0.1.0\", tools = {{\"t\": tool}}, hooks = [{hooks}])\n"
    );
    let data = tempfile::tempdir().expect("data root");
    let dir = data.path().join("plugins").join("probe");
    std::fs::create_dir_all(&dir).expect("plugin dir");
    std::fs::write(dir.join("plugin.star"), source).expect("plugin source");
    let roots = LoadRoots {
        data_root: data.path().to_path_buf(),
        bundled: Vec::new(),
    };
    let generation = load(
        &roots,
        &PluginsConfig {
            enabled: Vec::new(),
            limits: PluginLimits::default(),
            configs: BTreeMap::new(),
        },
    )
    .expect("multi-hook plugin loads");
    let dump = generation.registrations();
    let hook_lines: Vec<&str> = dump
        .lines()
        .filter(|line| line.starts_with("hook "))
        .collect();
    assert_eq!(
        hook_lines.len(),
        count,
        "every hook must register once: {dump}"
    );
    let mut seen = BTreeSet::new();
    for line in hook_lines {
        // Registration lines render `hook <plugin>/<event>,#<seq>,uses=...`;
        // the generated local identity is `<event>-<seq>`.
        let head = line.split(',').next().expect("hook line has a head");
        let identity = head
            .split_whitespace()
            .nth(1)
            .expect("hook line names its identity");
        let event = identity
            .split('/')
            .nth(1)
            .expect("hook identity names its event");
        let seq: usize = line
            .split('#')
            .nth(1)
            .and_then(|tail| tail.split(',').next())
            .and_then(|digits| digits.parse().ok())
            .expect("hook line carries its sequence number");
        let local = format!("{event}-{seq}");
        assert!(
            valid_generated_name(&local),
            "generated name must pass grammar: {local}"
        );
        assert!(seen.insert(local), "generated names must be unique");
    }
}
