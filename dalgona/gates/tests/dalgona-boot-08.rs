// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
#[path = "support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use dal_agent::ext::{BoxFuture, CommandCx, CommandHandler, ExtensionBuilder};
use dal_agent::{Env, Host, ServiceError};
use dal_core::ext::{CommandName, CommandSpec};
use dal_core::{Config, ConfigProduct, Origin, Reply, ServiceSet};

#[test]
fn every_battery_is_a_bundled_rust_extension_and_disabling_one_removes_only_it()
-> support::TestResult<()> {
    let scratch = support::Scratch::new("battery-inventory")?;
    let root = scratch.path().to_path_buf();
    let product = support::build_product(root.clone(), None)?;
    assert!(product.bundled.is_empty(), "no Starlark source ships in the product");
    let full = support::battery_names(&product);
    assert_eq!(full.len(), support::BATTERIES.len());
    for extension in &product.extensions {
        if full.contains(extension.name()) {
            assert_eq!(extension.origin(), Origin::Bundled, "{}", extension.name());
        }
        if extension.name() == "dalgona" {
            assert_eq!(extension.origin(), Origin::Builtin, "manual docs origin");
        }
    }
    for battery in support::BATTERIES {
        let toml = format!("disabled_batteries = [\"{battery}\"]\n");
        let reduced = support::build_product(root.clone(), Some(&toml))?;
        let mut expected = full.clone();
        expected.remove(battery);
        assert_eq!(support::battery_names(&reduced), expected, "disabling {battery}");
        assert!(
            reduced.extensions.iter().any(|extension| extension.name() == "dalgona"),
            "the manual stays when {battery} is disabled"
        );
    }
    Ok(())
}

struct Handler;

impl CommandHandler for Handler {
    fn run<'a>(
        &'a self,
        _args: &'a str,
        _cx: CommandCx<'a>,
    ) -> BoxFuture<'a, Result<Reply, ServiceError>> {
        Box::pin(async { Ok(Reply::Queued) })
    }
}

#[tokio::test]
async fn plan_command_clash_is_a_load_error_naming_both_owners()
-> support::TestResult<()> {
    let scratch = support::Scratch::new("plan-command-clash")?;
    let root = scratch.path().to_path_buf();
    let factory = dalgona::product();
    let config = Config::load(ConfigProduct::Dalgona, &root, factory.defaults, None)?;
    let cx = dalgon::BuildCx {
        data_root: root.clone(),
        config: &config,
    };
    let mut product = dalgona::build(&cx)?;
    let extension = ExtensionBuilder::new("plan", "0.1.0", ServiceSet::EMPTY)?
        .with_origin(Origin::User, None)
        .command(
            CommandSpec {
                name: CommandName::parse("plan")?,
                summary: "Conflicting plan command".into(),
                args_hint: None,
            },
            Arc::new(Handler),
        )
        .build()?;
    product.extensions.push(extension);
    let env = Env {
        vars: BTreeMap::new(),
        cwd: root,
        sandbox_helper: None,
    };
    let error = match Host::start(product, config, env).await {
        Ok(host) => {
            let _ = host.shutdown(std::time::Duration::from_secs(2)).await;
            return Err("a duplicate /plan command was accepted".into());
        }
        Err(error) => error.to_string(),
    };
    assert!(error.contains("plan"), "{error}");
    assert!(error.contains("work"), "{error}");
    Ok(())
}
