// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate tests.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]

use dal_core::Origin;
use dalgona_gates::support;
use std::{collections::BTreeSet, process::Command, time::Duration};

/// The symbol-off refusal as the journal record's JSON encoding carries it.
const SYMBOL_OFF: &str =
    "search: mode \\\"symbol\\\" is off. Set search_symbols = true in dal.toml.";

/// Runs one scripted `dalgona -p` turn with `judged` enabled and returns its
/// `search`, `patch`, and `todo` results in call order.
///
/// The provider opens are the judged default sequence: the round-zero
/// reasoning classification (answer 0 picks the first offered level), then
/// one step per model round. Later rounds reuse the classified level, the
/// short final reply stays under the claim-check floor, and no call is to
/// `ask`, so no further judge call opens.
fn three_tool_results(label: &str, top_level_toml: &str) -> support::TestResult<Vec<String>> {
    let scratch = support::Scratch::new(label)?;
    std::fs::write(
        scratch.path().join("probe.rs"),
        "pub fn gate_config_probe() -> u64 {\n    41\n}\n",
    )?;
    let journal = support::run_scripted_print(
        &scratch,
        top_level_toml,
        &[
            support::text_step(r#"{"answers":[0]}"#),
            support::tool_step(
                "s1",
                "search",
                r#"{"mode":"symbol","pattern":"gate_config_probe"}"#,
            ),
            support::tool_step("p1", "patch", r#"{"input":"this is not a patch"}"#),
            support::tool_step(
                "t1",
                "todo",
                r#"{"action":"write","todos":[{"subject":"Gate config probe","state":"in_progress"}]}"#,
            ),
            support::text_step("done"),
        ],
        "probe the configuration",
    )?;
    let results: Vec<String> = support::journal_records(&journal, "tool_result")
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(results.len(), 3, "{journal}");
    Ok(results)
}

#[test]
fn the_real_process_runs_dalgona_product_defaults_and_user_file_values() -> support::TestResult<()>
{
    // Dalgona product defaults with every battery enabled: symbol search
    // answers, patch speaks hashline, and the work battery's todo tool is
    // registered.
    let defaults = three_tool_results("config-defaults", "")?;
    let [search, patch, todo] = defaults.as_slice() else {
        return Err("three default tool results".into());
    };
    assert!(search.contains(r#""error":false"#), "{search}");
    assert!(search.contains("probe.rs"), "{search}");
    assert!(
        patch.contains(r#"Current edit_style is \"hashline\""#),
        "{patch}"
    );
    assert!(todo.contains(r#""error":false"#), "{todo}");
    assert!(!todo.contains("unknown tool"), "{todo}");

    // A user dal.toml overrides each named value in the same binary: symbol
    // mode refuses, patch speaks anchor, and the user battery list removes
    // work's todo tool.
    let overridden = three_tool_results(
        "config-overrides",
        "disabled_batteries = [\"work\"]\nsearch_symbols = false\nedit_style = \"anchor\"",
    )?;
    let [search, patch, todo] = overridden.as_slice() else {
        return Err("three overridden tool results".into());
    };
    assert!(search.contains(SYMBOL_OFF), "{search}");
    assert!(
        patch.contains(r#"Current edit_style is \"anchor\""#),
        "{patch}"
    );
    assert!(!patch.contains("hashline"), "{patch}");
    assert!(
        todo.contains("unknown tool: todo") || todo.contains("unknown tool todo."),
        "{todo}"
    );
    Ok(())
}

#[test]
fn the_real_process_rejects_an_unknown_user_key_before_any_session() -> support::TestResult<()> {
    let scratch = support::Scratch::new("config-unknown-key")?;
    let config_home = scratch.path().join("xdg-config");
    let data_home = scratch.path().join("xdg-data");
    std::fs::create_dir_all(config_home.join("dalgona"))?;
    std::fs::write(
        config_home.join("dalgona/dal.toml"),
        "model = \"openai-responses/gpt-6\"\nsearch_symbol = true\n",
    )?;
    let output = support::run_command(
        Command::new(support::dalgona_binary()?)
            .args(["-p", "--json", "--approval", "all", "unused"])
            .current_dir(scratch.path())
            .env_clear()
            .env("HOME", scratch.path())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", &data_home),
    )?;
    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    // The diagnostic names this product, the rejected key, and the key the
    // user meant; the surrounding wording is not part of the contract.
    let first = stderr.lines().next().unwrap_or_default();
    assert!(first.starts_with("dalgona: "), "{stderr}");
    assert!(first.contains("\"search_symbol\""), "{stderr}");
    assert!(stderr.contains("\"search_symbols\""), "{stderr}");
    assert!(output.stdout.is_empty(), "a rejected config prints no turn");
    assert!(
        !data_home.join("dalgona/sessions").exists(),
        "a rejected config opens no session"
    );
    Ok(())
}

#[test]
fn product_registers_every_bundled_battery() -> support::TestResult<()> {
    let scratch = support::Scratch::new("dalgona-battery-set")?;
    let data_root = scratch.path().to_path_buf();
    let product = support::build_product(data_root.clone(), None)?;
    let battery_names = support::battery_names(&product);
    assert_eq!(battery_names, BTreeSet::from(support::BATTERIES));
    assert!(
        product.bundled.is_empty(),
        "no Starlark source ships in the product"
    );
    for extension in &product.extensions {
        if support::BATTERIES.contains(&extension.name()) {
            assert_eq!(extension.origin(), Origin::Bundled, "{}", extension.name());
        }
        if extension.name() == "dalgona" {
            assert_eq!(extension.origin(), Origin::Builtin, "manual docs origin");
        }
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(data_root).await?;
        let page = host.doc("dalgona://batteries")?;
        let view = format!("{page:?}");
        for name in support::BATTERIES {
            assert!(view.contains(name), "battery inventory omits {name}");
        }
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
