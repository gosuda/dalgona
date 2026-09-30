//! One validated plugin generation and its read-only snapshot holder.
//!
//! The generation is the adapter's load result: every plugin's frozen module
//! plus its validated exports, commands, hooks, skills, rules and models.
//! Host publication stays with `Host::publish_plugins`; this holder only
//! serves the generation snapshotted at a turn start to callers inside this
//! crate.

use std::sync::{Arc, RwLock};

use dal_core::Origin;

use crate::validate::{ExportBody, LoadedPlugin};

/// One validated load result shared by every session of a turn.
#[derive(Debug)]
pub struct PluginGeneration {
    /// A fresh v7 identifier per successful load.
    pub id: uuid::Uuid,
    /// Loaded plugins in `(origin rank, name)` order.
    pub(crate) plugins: Box<[Arc<LoadedPlugin>]>,
}

impl PluginGeneration {
    /// Returns the number of loaded plugins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Whether this generation has no loaded plugins.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Iterates over loaded plugin names in generation order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.plugins.iter().map(|plugin| plugin.name.as_str())
    }

    /// Builds a generation from validated load output.
    #[must_use]
    pub(crate) fn new(plugins: Box<[Arc<LoadedPlugin>]>) -> Self {
        Self {
            id: uuid::Uuid::now_v7(),
            plugins,
        }
    }

    /// An empty generation used when no plugins load.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(Box::new([]))
    }

    /// Renders every registration as one sorted line per record, for
    /// fixture comparison and `dal://plugins` diagnostics.
    #[must_use]
    pub fn registrations(&self) -> String {
        let mut lines = Vec::new();
        for entry in &self.plugins {
            let origin = match entry.origin {
                Origin::Bundled => "bundled",
                Origin::User => "user",
                _ => "other",
            };
            lines.push(format!(
                "plugin {}/{},{}",
                entry.name.as_str(),
                entry.version.as_ref(),
                origin
            ));
            for export in &entry.exports {
                let visibility = match &export.body {
                    ExportBody::Tool { visibility, .. } => match visibility {
                        dal_core::Visibility::Deferred => "deferred",
                        dal_core::Visibility::EvalOnly => "eval_only",
                        _ => "model",
                    },
                };
                lines.push(format!(
                    "{} {}/{},{},uses={}",
                    export.id.kind.as_str(),
                    entry.name.as_str(),
                    export.id.local.as_str(),
                    visibility,
                    uses_text(&export.uses)
                ));
            }
            for command in &entry.commands {
                lines.push(format!(
                    "command {},uses={}",
                    command.spec.name.as_str(),
                    uses_text(&command.tool.uses)
                ));
            }
            for skill in &entry.skills {
                lines.push(format!(
                    "skill {}/{}",
                    entry.name.as_str(),
                    skill.name.as_str()
                ));
            }
            for rule in &entry.rules {
                lines.push(format!(
                    "rule {}/{}",
                    entry.name.as_str(),
                    rule.name.as_str()
                ));
            }
            for hook in &entry.hooks {
                lines.push(format!(
                    "hook {}/{},#{},uses={}",
                    entry.name.as_str(),
                    hook.event.as_str(),
                    hook.seq,
                    uses_text(&hook.uses)
                ));
            }
            for model in &entry.models {
                lines.push(format!(
                    "model {}/{}",
                    entry.name.as_str(),
                    model.id.local.as_str()
                ));
            }
            if entry.prompt.is_some() {
                lines.push(format!("section {}", entry.name.as_str()));
            }
        }
        lines.sort();
        lines.join("\n")
    }
}

/// Renders a declared operation set as `a+b`, in set order.
fn uses_text(uses: &dal_core::ext::OpSet) -> String {
    uses.iter()
        .map(|op| op.to_string())
        .collect::<Vec<_>>()
        .join("+")
}

/// Loader state: the last validated generation plus its roots and budgets.
///
/// Reload re-runs the load pipeline and conversion, swaps the snapshot only
/// on full success, and reports whole-generation counts. Host publication of
/// the runtime snapshot stays with the host; this mirrors the adapter view
/// the next loader consumer reads. The signature matches the command
/// `PluginReload` seam exactly so its adapter is one delegation line.
#[derive(Debug)]
pub struct PluginSystem {
    current: RwLock<Arc<PluginGeneration>>,
    roots: crate::load::LoadRoots,
    cfg: crate::load::PluginsConfig,
}

impl PluginSystem {
    /// Holds one validated generation with the roots and budgets a reload
    /// re-reads; one holder serves every session.
    #[must_use]
    pub fn new(
        generation: PluginGeneration,
        roots: crate::load::LoadRoots,
        cfg: crate::load::PluginsConfig,
    ) -> Self {
        Self {
            current: RwLock::new(Arc::new(generation)),
            roots,
            cfg,
        }
    }

    /// Returns the generation visible to a turn starting now.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Poisoned`] when a writer panicked while holding
    /// the lock; an empty generation would silently serve stale callers.
    pub fn snapshot(&self) -> Result<Arc<PluginGeneration>, crate::LoadError> {
        self.current
            .read()
            .map(|guard| Arc::clone(&guard))
            .map_err(|_| crate::LoadError::Poisoned {
                what: "plugin generation".into(),
            })
    }

    /// Converts the snapshot into per-plugin extensions in canonical order:
    /// bundled plugins by name, then user plugins by name. The host appends
    /// the batch after the product list at startup and re-runs it through
    /// validation before publishing.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Registration`] at the offending declaration site.
    pub fn extensions(&self) -> Result<Vec<dal_agent::ext::Extension>, crate::LoadError> {
        crate::convert::convert_all(&*self.snapshot()?)
    }

    /// Converts the current snapshot and hands over the user-origin plugin
    /// extensions for host publication: the `/reload` one-path flow calls
    /// [`reload`](Self::reload) first, then passes this set to the host
    /// publication entry point. Bundled entries stay with the host's
    /// current set because their frozen modules are reused across reloads.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Registration`] at the offending declaration site.
    pub fn user_extensions(&self) -> Result<Vec<dal_agent::ext::Extension>, crate::LoadError> {
        Ok(crate::convert::convert_all(&*self.snapshot()?)?
            .into_iter()
            .filter(|ext| ext.origin() == Origin::User)
            .collect())
    }

    /// Re-runs load and conversion, swaps the snapshot only on full
    /// success, and reports whole-generation counts. A failure keeps the
    /// old snapshot and arrives as a command triple.
    ///
    /// # Errors
    ///
    /// Returns a command error triple if loading or conversion fails.
    pub fn reload(
        &self,
    ) -> Result<dal_agent::ext::command::ReloadOk, dal_core::command::ErrorTriple> {
        let generation = match crate::load::load(&self.roots, &self.cfg) {
            Ok(generation) => generation,
            Err(error) => {
                return Err(dal_core::command::ErrorTriple {
                    what: Box::from("plugin reload"),
                    why: error
                        .render()
                        .lines()
                        .next()
                        .unwrap_or("plugin load failed")
                        .to_owned()
                        .into_boxed_str(),
                    fix: Box::from("edit the failing plugin and run /reload again"),
                });
            }
        };
        if let Err(error) = crate::convert::convert_all(&generation) {
            return Err(dal_core::command::ErrorTriple {
                what: Box::from("plugin reload"),
                why: error
                    .render()
                    .lines()
                    .next()
                    .unwrap_or("plugin conversion failed")
                    .to_owned()
                    .into_boxed_str(),
                fix: Box::from("edit the failing plugin and run /reload again"),
            });
        }
        let counts = generation_counts(&generation);
        match self.current.write() {
            Ok(mut current) => {
                *current = Arc::new(generation);
            }
            Err(_) => {
                return Err(dal_core::command::ErrorTriple {
                    what: Box::from("plugin reload"),
                    why: Box::from("the plugin generation lock is poisoned"),
                    fix: Box::from("restart the session and run /reload again"),
                });
            }
        }
        Ok(counts)
    }
}

/// Counts one generation for the reload reply.
fn generation_counts(generation: &PluginGeneration) -> dal_agent::ext::command::ReloadOk {
    let mut tools = 0_u64;
    let mut commands = 0_u64;
    let mut skills = 0_u64;
    for entry in &generation.plugins {
        tools += u64::try_from(entry.exports.len()).unwrap_or(u64::MAX);
        commands += u64::try_from(entry.commands.len()).unwrap_or(u64::MAX);
        skills += u64::try_from(entry.skills.len()).unwrap_or(u64::MAX);
    }
    dal_agent::ext::command::ReloadOk {
        plugins: u64::try_from(generation.plugins.len()).unwrap_or(u64::MAX),
        tools,
        commands,
        skills,
    }
}
