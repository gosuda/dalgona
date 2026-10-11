//! Deterministic rule discovery, precedence, compilation, and bucket selection.
//!
//! A [`RuleSet`] is the immutable snapshot shared with a session and its child
//! agents. The builder reads only the product data root and workspace supplied
//! by its caller; loaded plugins are an explicit snapshot, never process-global
//! state.

mod load;
mod snapshot;
#[cfg(test)]
mod tests;
mod view;

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

use dal_core::{RulesConfig, ext::RuleFile};

use super::record::RecordSource;
use super::value::{Problem, Rule};
use load::{BuildContext, DirectoryOrigin, add_plugin_rule_files, add_record, add_rule_directory};
use snapshot::BuildState;
use view::view_for_agent;

const FILE_LIMIT: usize = 65_536;
const DIRECTORY_LIMIT: usize = 256;
const STREAM_CONDITION_LIMIT: usize = 256;
const ALWAYS_BODY_LIMIT: usize = 12_000;
const ALWAYS_TOTAL_LIMIT: usize = 40_000;
const SKIPPED: &str = "dalgon skipped it.";

/// One immutable, agent-specific view of a discovered rule snapshot.
///
/// `candidates` retains every post-precedence winner, including rules excluded
/// from this view by their `agents` glob. [`for_agent`] uses that retained
/// snapshot to build a child view without reading files or losing child rules.
#[must_use]
#[derive(Clone, Debug)]
pub struct RuleSet {
    /// Rules that match the live stream, sorted by name.
    pub stream: Vec<Arc<Rule>>,
    /// Bodies included in every request, sorted by name.
    pub always: Vec<Arc<Rule>>,
    /// Described rules listed for reference, sorted by name.
    pub rulebook: Vec<Arc<Rule>>,
    /// Ordered discovery, validation, precedence, and bucket diagnostics.
    pub problems: Vec<Problem>,
    state: Arc<BuildState>,
}

/// The immutable registry inputs used to construct a rule snapshot.
#[derive(Clone, Copy, Debug)]
pub struct RuleBuildInput<'a> {
    /// Plugin-code rules with their registering plugin and source site.
    pub records: &'a [RecordSource],
    /// Markdown rule files emitted by the loaded plugin generation.
    pub plugin_rules: &'a [RuleFile],
    /// Tool names registered by the product.
    pub known_tools: &'a [&'a str],
    /// Agent name used for agent-scoped rules.
    pub agent: &'a str,
}

/// Builds a rule snapshot for the supplied rule records and plugin files.
///
/// `records` carries each record's registering plugin explicitly.
/// `plugin_rules` is the ordered snapshot of Markdown files under plugin
/// `rules/` directories. `known_tools` is the product's loaded tool-name
/// snapshot; unknown named tools receive a Note while their scope remains.
/// The product-specific data root and workspace are supplied by the caller;
/// this function never consults ambient process state or another product's
/// root.
///
/// User files precede plugins, each plugin's records precede its files, and
/// project files come last. The returned view is filtered for `agent`; its
/// private candidate snapshot retains matching-agent alternatives for
/// [`for_agent`].
pub fn set_for(
    input: &RuleBuildInput<'_>,
    data_root: &Path,
    workspace: &Path,
    cfg: &RulesConfig,
) -> RuleSet {
    let records = input.records;
    let plugin_rules = input.plugin_rules;
    let known_tools = input.known_tools;
    let agent = input.agent;
    let mut builder = BuildContext {
        cfg,
        known_tools,
        disabled: cfg.disabled.iter().map(Box::<str>::as_ref).collect(),
        winners: Vec::new(),
        by_name: HashMap::new(),
        problems: Vec::new(),
    };

    let user_dir = data_root.join("rules");
    add_rule_directory(&user_dir, DirectoryOrigin::User, &mut builder);

    let mut plugin_names = BTreeSet::new();
    let mut records_by_plugin: HashMap<&str, Vec<&RecordSource>> = HashMap::new();
    for source in records {
        plugin_names.insert(source.plugin.as_str().to_owned());
        records_by_plugin
            .entry(source.plugin.as_str())
            .or_default()
            .push(source);
    }
    let mut rules_by_plugin: HashMap<&str, Vec<&RuleFile>> = HashMap::new();
    for rule_file in plugin_rules {
        plugin_names.insert(rule_file.plugin.as_str().to_owned());
        rules_by_plugin
            .entry(rule_file.plugin.as_str())
            .or_default()
            .push(rule_file);
    }

    for plugin in &plugin_names {
        if let Some(plugin_records) = records_by_plugin.get_mut(plugin.as_str()) {
            plugin_records.sort_by(|left, right| {
                left.record
                    .name
                    .as_str()
                    .cmp(right.record.name.as_str())
                    .then_with(|| {
                        left.site
                            .path
                            .as_os_str()
                            .as_encoded_bytes()
                            .cmp(right.site.path.as_os_str().as_encoded_bytes())
                    })
                    .then(left.site.line.cmp(&right.site.line))
                    .then(left.site.col.cmp(&right.site.col))
            });
            for source in plugin_records.iter().copied() {
                add_record(source, &mut builder);
            }
        }

        if let Some(plugin_rule_files) = rules_by_plugin.get_mut(plugin.as_str()) {
            plugin_rule_files
                .sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
            add_plugin_rule_files(plugin_rule_files, &mut builder);
        }
    }

    let project_dir = workspace.join(".dal").join("rules");
    add_rule_directory(&project_dir, DirectoryOrigin::Project, &mut builder);

    let state = Arc::new(BuildState::new(
        builder.winners,
        builder.problems,
        cfg.watch,
    ));
    view_for_agent(state, agent)
}

/// Builds another agent's view from the retained snapshot without filesystem
/// access. Agent filtering is repeated before the name-ordered bucket walk.
pub fn for_agent(set: &RuleSet, agent: &str) -> RuleSet {
    view_for_agent(Arc::clone(&set.state), agent)
}
