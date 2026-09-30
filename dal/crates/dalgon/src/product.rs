//! The dal product identity and its built-in extension composition.

use std::sync::{Arc, atomic::AtomicBool};

use dal_agent::Product;
use dal_agent::ext::{Extension, ExtensionBuilder, PromptOrder, PromptSection};
use dal_core::PROMPT_DIAGRAMS;
use dal_tools::{Calibration, GuardConfig, ToolsConfig, guard_extension};

use crate::{BuildCx, BuildError, ProductFactory};

const BINARY: &str = "dalgon";
const NAME: &str = "dal";
const DEFAULTS: &str = "";

/// The dal extensions that a product variant may customize before assembly.
pub struct Parts {
    /// The single native tool extension configuration.
    pub tools: ToolsConfig,
    /// The core guard extension wired to the native tools observer.
    pub guard: dal_agent::ext::Extension,
    /// Product Rust batteries in name byte order (dal: empty).
    pub batteries: Vec<dal_agent::ext::Extension>,
    /// Embedded Starlark sources (dal: empty).
    pub bundled: Vec<dal_core::PluginSource>,
}

impl std::fmt::Debug for Parts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let batteries: Vec<&str> = self
            .batteries
            .iter()
            .map(dal_agent::ext::Extension::name)
            .collect();
        formatter
            .debug_struct("Parts")
            .field("tools", &self.tools)
            .field("guard", &self.guard.name())
            .field("batteries", &batteries)
            .field("bundled", &self.bundled.len())
            .finish()
    }
}

/// Builds the configurable dal tool and guard components.
///
/// # Errors
/// Returns the guard section's typed error or a registration error.
pub fn parts(cx: &BuildCx<'_>) -> Result<Parts, BuildError> {
    let guard_config =
        GuardConfig::from_section(cx.config.guard(), &Calibration::none()).map_err(|source| {
            BuildError::Section {
                section: "guard".into(),
                source: Box::new(source),
            }
        })?;
    let guard = guard_extension(guard_config)?;

    let mut tools = ToolsConfig::default();
    tools.search_symbols = Arc::new(AtomicBool::new(cx.config.search_symbols()));
    tools.index_root = Some(cx.data_root.join("index"));
    tools.edit_style = cx.config.edit_style().clone();
    tools.exec.sandbox_on = cx.config.sandbox();
    tools.observer = Some(Arc::clone(&guard.observer));

    Ok(Parts {
        tools,
        guard: guard.extension,
        batteries: Vec::new(),
        bundled: Vec::new(),
    })
}

struct ReloadPlugins(Arc<dal_star::PluginSystem>);
impl dal_ext::commands::PluginReload for ReloadPlugins {
    fn reload<'a>(
        &'a self,
        cx: &'a dal_agent::ext::command::CommandCx<'a>,
    ) -> dal_agent::ext::BoxFuture<
        'a,
        Result<dal_agent::ext::command::ReloadSummary, dal_core::command::ErrorTriple>,
    > {
        Box::pin(async move {
            self.0.reload()?;
            let set = self
                .0
                .user_extensions()
                .map_err(|error| dal_core::command::ErrorTriple {
                    what: "plugin reload".into(),
                    why: error
                        .render()
                        .lines()
                        .next()
                        .unwrap_or("plugin conversion failed")
                        .to_owned()
                        .into(),
                    fix: "edit the failing plugin and run /reload again".into(),
                })?;
            cx.publish_plugins(set)
                .await
                .map_err(dal_ext::commands::misc::publish_failure)
        })
    }
}

/// Assembles a dal product from its base parts and fixed extension set.
///
/// # Errors
/// Returns a registration error when a built-in extension cannot be registered.
pub fn assemble(cx: &BuildCx<'_>, parts: Parts) -> Result<Product, BuildError> {
    let roots = dal_star::LoadRoots {
        data_root: cx.data_root.clone(),
        bundled: parts
            .bundled
            .iter()
            .map(|source| dal_star::BundledPlugin {
                name: source.name.to_string(),
                files: source.files.clone(),
            })
            .collect(),
    };
    let plugincfg = dal_star::PluginsConfig {
        enabled: cx
            .config
            .plugins()
            .iter()
            .map(std::string::ToString::to_string)
            .collect(),
        limits: *cx.config.plugin_limits(),
        configs: cx
            .config
            .plugin_configs()
            .map(|(name, table)| Ok((name.to_owned(), sonic_rs::to_string(table)?)))
            .collect::<Result<_, sonic_rs::Error>>()
            .map_err(|source| BuildError::Section {
                section: "plugin".into(),
                source: Box::new(source),
            })?,
    };
    let generation = dal_star::load(&roots, &plugincfg).map_err(|source| BuildError::Section {
        section: "plugins".into(),
        source: Box::new(source),
    })?;
    let system = Arc::new(dal_star::PluginSystem::new(generation, roots, plugincfg));
    let mut extensions = vec![
        dal_tools::extension(parts.tools)?,
        parts.guard,
        dal_ext::prompt::extension()?,
        dal_ext::skills::extension()?,
        dal_ext::letter::extension()?,
        dal_ext::ttsr::extension()?,
        dal_ext::compact::extension()?,
        dal_ext::commands::extension(Arc::new(ReloadPlugins(Arc::clone(&system))))?,
        dal_ext::docs::extension()?,
        dal_ext::subagent::extension()?,
    ];
    if cx.config.tui().diagrams {
        extensions.push(diagrams_prompt_extension()?);
    }
    let limits = cx.config.plugin_limits();
    extensions.push(dal_star::eval_extension(dal_star::Limits {
        ticks: limits.cell_ticks,
        heap_bytes: limits.cell_heap_bytes,
        stack_depth: limits.stack_depth,
    })?);
    extensions.extend(parts.batteries);
    let batch = system.extensions().map_err(|source| BuildError::Section {
        section: "plugins".into(),
        source: Box::new(source),
    })?;
    extensions.extend(batch);

    Ok(Product {
        name: NAME,
        data_root: cx.data_root.clone(),
        defaults: DEFAULTS,
        extensions,
        bundled: parts.bundled,
    })
}

fn diagrams_prompt_extension() -> Result<Extension, dal_core::RegistrationError> {
    ExtensionBuilder::new("diagram-prompt", "0.1.0", dal_core::ServiceSet::EMPTY)?
        .prompt_section(PromptSection::static_text(
            PromptOrder::D2,
            PROMPT_DIAGRAMS.into(),
        ))
        .build()
}

/// Builds dal's product using the default dal extension composition.
///
/// # Errors
/// Returns an extension-owned configuration or registration error.
pub fn build(cx: &BuildCx<'_>) -> Result<Product, BuildError> {
    assemble(cx, parts(cx)?)
}

/// Returns the first-party dal manual pages for command-line documentation.
#[must_use]
pub fn builtin_manuals() -> Vec<crate::ProductManual> {
    dal_ext::docs::snapshot().manuals
}

/// Returns the dal factory used by every dalgon binary alias.
#[must_use]
pub fn product() -> ProductFactory {
    ProductFactory {
        binary: BINARY,
        defaults: DEFAULTS,
        build,
        docs: builtin_manuals,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dal_core::{Config, ConfigProduct};

    #[test]
    fn factory_preserves_dal_identity_and_extension_order() {
        let dir = tempfile::tempdir().unwrap();
        let data_root = dir.path().to_path_buf();
        let config = Config::load(ConfigProduct::Dalgon, &data_root, DEFAULTS, None).unwrap();
        let cx = BuildCx {
            data_root: data_root.clone(),
            config: &config,
        };

        let factory = product();
        assert_eq!(factory.binary, "dalgon");
        assert_eq!(factory.defaults, "");

        let built = (factory.build)(&cx).unwrap();
        assert_eq!(built.name, "dal");
        assert_eq!(built.data_root, data_root);
        assert_eq!(built.defaults, "");
        assert!(built.bundled.is_empty());
        assert!(!config.search_symbols());
        assert!(!config.guard().enabled);

        let names: Vec<_> = built
            .extensions
            .iter()
            .map(|extension| extension.name().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "tools", "guard", "prompt", "skills", "letter", "ttsr", "compact", "commands",
                "dal", "subagent", "eval",
            ]
        );

        let repeated = build(&cx).unwrap();
        let repeated_names: Vec<_> = repeated
            .extensions
            .iter()
            .map(|extension| extension.name().to_string())
            .collect();
        assert_eq!(repeated_names, names);
    }

    #[test]
    fn diagram_prompt_is_appended_once_only_when_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let data_root = dir.path().to_path_buf();
        for (toml, enabled) in [("", false), ("[tui]\ndiagrams = true\n", true)] {
            let config =
                Config::load(ConfigProduct::Dalgon, &data_root, DEFAULTS, Some(toml)).unwrap();
            let cx = BuildCx {
                data_root: data_root.clone(),
                config: &config,
            };
            let built = build(&cx).unwrap();
            let sections: Vec<_> = built
                .extensions
                .iter()
                .filter_map(dal_agent::ext::Extension::prompt_section)
                .collect();
            let occurrences = sections
                .iter()
                .filter(|section| {
                    matches!(
                        section,
                        PromptSection::Static { text, order: PromptOrder::D2, .. }
                            if text.as_ref() == PROMPT_DIAGRAMS
                    )
                })
                .count();
            assert_eq!(occurrences, if enabled { 1 } else { 0 });
        }
    }

    #[test]
    fn factory_appends_configured_plugin_after_fixed_order() {
        let dir = tempfile::tempdir().unwrap();
        let data_root = dir.path().to_path_buf();
        let plugin_dir = data_root.join("plugins").join("focus");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.star"),
            "load(\"@dal/v1\", \"dal\")\n\ndef focus(ctx, args):\n    return args\n\nplugin = dal.plugin(\n    name = \"focus\",\n    version = \"0.1.0\",\n    tools = {\"focus\": dal.tool(description = \"Return the supplied focus data.\", input = dal.schema(), run = focus)},\n)\n",
        )
        .unwrap();
        let config = Config::load(ConfigProduct::Dalgon, &data_root, DEFAULTS, None).unwrap();
        let cx = BuildCx {
            data_root: data_root.clone(),
            config: &config,
        };

        let built = build(&cx).unwrap();
        let names: Vec<_> = built
            .extensions
            .iter()
            .map(|extension| extension.name().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "tools", "guard", "prompt", "skills", "letter", "ttsr", "compact", "commands",
                "dal", "subagent", "eval", "focus",
            ]
        );
    }

    #[test]
    fn factory_orders_batteries_then_bundled_after_builtins() {
        const FOCUS_STAR: &[u8] = b"load(\"@dal/v1\", \"dal\")\n\ndef focus(ctx, args):\n    return args\n\nplugin = dal.plugin(\n    name = \"focus\",\n    version = \"0.1.0\",\n    tools = {\"focus\": dal.tool(description = \"Return the supplied focus data.\", input = dal.schema(), run = focus)},\n)\n";
        let dir = tempfile::tempdir().unwrap();
        let data_root = dir.path().to_path_buf();
        let config = Config::load(ConfigProduct::Dalgon, &data_root, DEFAULTS, None).unwrap();
        let cx = BuildCx {
            data_root: data_root.clone(),
            config: &config,
        };

        let mut partial = parts(&cx).unwrap();
        assert!(partial.batteries.is_empty());
        assert!(partial.bundled.is_empty());
        let battery =
            dal_agent::ext::ExtensionBuilder::new("battery", "0.1.0", dal_core::ServiceSet::EMPTY)
                .unwrap()
                .build()
                .unwrap();
        partial.batteries.push(battery);
        partial.bundled.push(dal_core::PluginSource {
            name: "focus".into(),
            files: [(std::path::PathBuf::from("plugin.star"), FOCUS_STAR)]
                .into_iter()
                .collect(),
        });

        let built = assemble(&cx, partial).unwrap();
        assert_eq!(built.bundled.len(), 1);
        assert_eq!(built.bundled[0].name.as_ref(), "focus");
        let names: Vec<_> = built
            .extensions
            .iter()
            .map(|extension| extension.name().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "tools", "guard", "prompt", "skills", "letter", "ttsr", "compact", "commands",
                "dal", "subagent", "eval", "battery", "focus",
            ]
        );
    }
}
