// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Every public builder URI resolves to its live product content.
use dalgona_gates::support;

use std::{io, time::Duration};

#[test]
fn bundled_rust_batteries_pass_the_public_builder() -> support::TestResult<()> {
    let scratch = support::Scratch::new("bundled-plugin-builder")?;
    let root = scratch.path().to_path_buf();
    let product = support::build_product(root.clone(), None)?;
    // Live product data the served pages must reflect: the registry, the
    // defaults leaves, the compiled rule names, and the skill records.
    let registry: Vec<String> = support::battery_names(&product)
        .into_iter()
        .map(str::to_owned)
        .collect();
    let table: toml::Table = dalgona::product().defaults.parse()?;
    let top_level: Vec<String> = table
        .iter()
        .filter(|(_, value)| !value.is_table())
        .map(|(key, _)| key.clone())
        .collect();
    let rules = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "ttsr-rules")
        .ok_or_else(|| io::Error::other("the product has no ttsr-rules extension"))?
        .rules()
        .iter()
        .map(|rule| rule.name.as_str().to_owned())
        .collect::<Vec<_>>();
    assert!(!rules.is_empty(), "the live product registers rules");
    let skills = product
        .extensions
        .iter()
        .find(|extension| extension.name() == "skills")
        .ok_or_else(|| io::Error::other("the product has no skills extension"))?
        .skills()
        .iter()
        .map(|skill| (skill.name.as_str().to_owned(), skill.body.to_string()))
        .collect::<Vec<_>>();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = support::start_dalgona(root).await?;
        // One-character documents cannot satisfy any cross-check below.
        let batteries = format!("{:?}", host.doc("dalgona://batteries")?);
        for name in &registry {
            assert!(
                batteries.contains(name),
                "served batteries page omits live battery {name}"
            );
        }
        let config = format!("{:?}", host.doc("dalgona://config")?);
        for key in &top_level {
            assert!(
                config.contains(key),
                "served config page omits live defaults key {key}"
            );
        }
        let rules_page = format!("{:?}", host.doc("dalgona://rules")?);
        for name in &rules {
            assert!(
                rules_page.contains(name),
                "served rules page omits live rule {name}"
            );
        }
        // The served pages carry the live skill bodies: the two skills no
        // other gate checks, plus the name of the third.
        for uri in ["skill://find-anything", "skill://delegate-with-contracts"] {
            let doc = format!("{:?}", host.doc(uri)?);
            let name = uri.strip_prefix("skill://").unwrap_or(uri);
            let (_, body) = skills
                .iter()
                .find(|(skill, _)| skill == name)
                .ok_or_else(|| io::Error::other(format!("live product has no skill {name}")))?;
            assert!(
                !body.is_empty(),
                "the live skill {name} carries a compiled-in body"
            );
            assert!(
                doc.contains(&format!("{body:?}")),
                "served skill page for {uri} carries the live body"
            );
        }
        let initializer = format!("{:?}", host.doc("skill://initializer-and-sprints")?);
        assert!(
            initializer.contains("initializer-and-sprints"),
            "served skill page names the live skill"
        );
        let missing = host.doc("skill://nope");
        assert_eq!(
            missing.err().map(|error| error.to_string()).as_deref(),
            Some("unknown skill: nope"),
            "an unknown skill URI is the typed unknown-skill error"
        );
        let report = host.shutdown(Duration::from_secs(2)).await;
        assert_eq!(report.sessions_closed, 0);
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    Ok(())
}
