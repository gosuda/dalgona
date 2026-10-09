// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Composition gates for the dalgona product.
#![expect(
    clippy::disallowed_methods,
    reason = "gate runs the real product binaries"
)]
use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use dal_agent::Product;
use dal_core::{Config, ConfigProduct};
use dalgon::BuildCx;

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct Root(PathBuf);

impl Root {
    fn create() -> io::Result<Self> {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "dalgona-composition-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn build(user_toml: &str) -> Result<Product, Box<dyn Error>> {
    let root = Root::create()?;
    let factory = dalgona::product();
    let config = Config::load(
        ConfigProduct::Dalgona,
        &root.0,
        factory.defaults,
        Some(user_toml),
    )?;
    let cx = BuildCx {
        data_root: root.0.clone(),
        config: &config,
    };
    Ok(dalgona::build(&cx)?)
}

fn names(product: &Product) -> BTreeSet<&str> {
    product
        .extensions
        .iter()
        .map(dal_agent::ext::Extension::name)
        .collect()
}

fn failure(user_toml: &str) -> Result<String, Box<dyn Error>> {
    match build(user_toml) {
        Ok(_) => Err(io::Error::other(format!("accepted: {user_toml}")).into()),
        Err(error) => Ok(error.to_string()),
    }
}

#[test]
fn diagram_prompt_tracks_the_strict_tui_setting() -> Result<(), Box<dyn Error>> {
    let root = Root::create()?;
    let factory = dalgona::product();
    for (user_toml, enabled) in [("", false), ("[tui]\ndiagrams = true\n", true)] {
        let config = Config::load(
            ConfigProduct::Dalgona,
            &root.0,
            factory.defaults,
            Some(user_toml),
        )?;
        assert_eq!(config.tui().diagrams, enabled);
        let cx = BuildCx {
            data_root: root.0.clone(),
            config: &config,
        };
        let product = dalgona::build(&cx)?;
        let occurrences = product
            .extensions
            .iter()
            .filter_map(dal_agent::ext::Extension::prompt_section)
            .filter(|section| matches!(
                section,
                dal_agent::ext::PromptSection::Static { text, order: dal_agent::ext::PromptOrder::D2, .. }
                    if text.as_ref() == dal_core::PROMPT_DIAGRAMS
            ))
            .count();
        assert_eq!(occurrences, usize::from(enabled));
    }
    Ok(())
}

#[test]
fn docs_command_serves_both_first_party_schemes() -> Result<(), Box<dyn Error>> {
    let root = Root::create()?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_dalgona"));
    command
        .env_clear()
        .env("HOME", &root.0)
        .env("XDG_CONFIG_HOME", root.0.join("config"))
        .env("XDG_DATA_HOME", root.0.join("data"));
    let page = command.args(["docs", "dalgona://config"]).output()?;
    assert_eq!(page.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&page.stdout).starts_with("# "));
    assert_eq!(page.stderr, [] as [u8; 0]);

    let mut command = Command::new(env!("CARGO_BIN_EXE_dalgona"));
    command
        .env_clear()
        .env("HOME", &root.0)
        .env("XDG_CONFIG_HOME", root.0.join("config"))
        .env("XDG_DATA_HOME", root.0.join("data"));
    let listing = command.args(["docs"]).output()?;
    assert_eq!(listing.status.code(), Some(0));
    let text = String::from_utf8_lossy(&listing.stdout);
    assert!(text.contains("dal://"));
    assert!(text.contains("dalgona://"));
    assert_eq!(listing.stderr, [] as [u8; 0]);
    Ok(())
}

#[test]
fn the_manual_is_one_extension_with_every_page() -> Result<(), Box<dyn Error>> {
    let product = build("")?;
    let manual = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "dalgona")
        .ok_or_else(|| io::Error::other("the product has no dalgona manual extension"))?;
    let pages: BTreeSet<&str> = manual.docs().iter().map(|page| &*page.path).collect();
    assert_eq!(
        pages,
        BTreeSet::from([
            "anti-slop",
            "ask",
            "batteries",
            "changelog",
            "config",
            "history",
            "judge",
            "judged",
            "mcp",
            "orchestration",
            "philosophy",
            "plan",
            "quality",
            "review",
            "rules",
            "search",
            "skills",
            "todo",
            "ttsr/rules",
            "web",
            "work",
        ])
    );
    for page in manual.docs() {
        assert!(page.text.starts_with("# "), "{} has no heading", page.path);
        assert!(!page.title.is_empty(), "{} has no title", page.path);
    }
    Ok(())
}

#[test]
fn a_bad_section_fails_the_build_and_names_the_bad_key() -> Result<(), Box<dyn Error>> {
    let cases = [
        ("[plugin.ask]\nmisspelled = true\n", "misspelled"),
        ("[plugin.skills]\nmisspelled = true\n", "misspelled"),
        ("[plugin.ttsr-rules]\nmisspelled = true\n", "misspelled"),
        ("[plugin.work]\nenabled = true\n", "plugin.plan"),
        ("[plugin.web]\nbogus = 1\n", "bogus"),
        (
            "[plugin.review]\nmax_rounds = 11\n",
            "plugin.review.max_rounds",
        ),
        ("[plugin.history]\nshare = 0.9\n", "share"),
        ("[plugin.judged]\nthinkin = true\n", "thinkin"),
        ("[plugin.mcp]\nservers = 1\n", "servers"),
        ("[plugin.quality]\nstrict = true\n", "strict"),
        ("[plugin.plan]\nverbose = true\n", "verbose"),
        ("[plugin.orchestration.goal]\nbogus = 1\n", "bogus"),
        ("[rule_sets]\nenabled = \"steer\"\n", "rule_sets.enabled"),
        ("[rule_sets]\nextra = 1\n", "extra"),
        ("[rule_sets]\nenabled = [\"nope\"]\n", "nope"),
    ];
    for (toml, fragment) in cases {
        let error = failure(toml)?;
        assert!(error.contains(fragment), "{toml:?} -> {error}");
    }
    Ok(())
}

#[test]
fn each_enabled_switch_removes_only_its_battery() -> Result<(), Box<dyn Error>> {
    let full = build("")?;
    let all = names(&full);
    for (battery, toml) in [
        ("web", "[plugin.web]\nenabled = false\n"),
        ("review", "[plugin.review]\nenabled = false\n"),
        ("mcp", "[plugin.mcp]\nenabled = false\n"),
        ("work", "[plugin.plan]\nenabled = false\n"),
    ] {
        let product = build(toml)?;
        let mut expected = all.clone();
        expected.remove(battery);
        assert_eq!(names(&product), expected, "{battery}");
    }
    Ok(())
}

#[test]
fn history_disabled_keeps_the_extension_without_a_compactor() -> Result<(), Box<dyn Error>> {
    let enabled = build("")?;
    let disabled = build("[plugin.history]\nenabled = false\n")?;
    let compactors = |product: &Product| {
        product
            .extensions
            .iter()
            .filter(|extension| extension.name() == "history")
            .map(|extension| extension.compactors().len())
            .sum::<usize>()
    };
    assert_eq!(compactors(&enabled), 1);
    assert_eq!(compactors(&disabled), 0);
    Ok(())
}

#[test]
fn the_guard_stays_when_quality_is_disabled() -> Result<(), Box<dyn Error>> {
    let with_quality = build("")?;
    let without = build("disabled_batteries = [\"quality\"]\n")?;
    let count = |product: &Product, name: &str| {
        product
            .extensions
            .iter()
            .filter(|extension| extension.name() == name)
            .count()
    };
    assert_eq!(count(&with_quality, "guard"), 1);
    assert_eq!(count(&with_quality, "quality"), 1);
    assert_eq!(count(&without, "guard"), 1);
    assert_eq!(count(&without, "quality"), 0);
    Ok(())
}

#[test]
fn batteries_register_in_name_byte_order_after_the_builtins() -> Result<(), Box<dyn Error>> {
    let product = build("")?;
    let batteries: Vec<&str> = product
        .extensions
        .iter()
        .filter(|extension| extension.origin() == dal_core::Origin::Bundled)
        .map(dal_agent::ext::Extension::name)
        .collect();
    let mut sorted = batteries.clone();
    sorted.sort_unstable();
    assert_eq!(batteries, sorted);
    assert_eq!(batteries.len(), 11);
    Ok(())
}
