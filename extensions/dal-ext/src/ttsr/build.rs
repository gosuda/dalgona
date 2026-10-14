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
use super::value::{Origin, Problem, Rule};
use load::{BuildContext, DirectoryOrigin, add_plugin_rule_files, add_record, add_rule_directory};
use snapshot::BuildState;
use view::view_for_agent;

const FILE_LIMIT: usize = 65_536;
const DIRECTORY_LIMIT: usize = 256;
const STREAM_CONDITION_LIMIT: usize = 256;
const ALWAYS_BODY_LIMIT: usize = 12_288;
const ALWAYS_TOTAL_LIMIT: usize = 40_960;
const RULEBOOK_LINE_LIMIT: usize = 4_096;
const RULEBOOK_LISTING_LIMIT: usize = 10_240;
const SKIPPED: &str = "dalgon skipped it.";

/// Note rendered when a rule's body alone exceeds the always-apply body cap.
pub const BODY_OVER_LIMIT_NOTE: &str =
    "the body is longer than 12288 bytes. dalgona lists this rule in the rulebook instead.";
/// Problem reason when a body over the cap also lacks a description.
pub const BODY_OVER_LIMIT_REASON: &str = "the body is longer than 12288 bytes";
/// Note rendered when the always-apply prompt section is full.
pub const ALWAYS_BUDGET_FULL_NOTE: &str = "the always-apply budget of 40960 bytes is full. dalgona lists this rule in the rulebook instead.";
/// Problem reason when the full always-apply budget rule also lacks a description.
pub const ALWAYS_BUDGET_FULL_REASON: &str = "the always-apply budget of 40960 bytes is full";

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
    agent: Box<str>,
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
    set_for_with_judged(
        input,
        data_root,
        workspace,
        cfg,
        cfg.judge != dal_core::JudgeMode::Off,
    )
}

/// Builds a complete retained rule snapshot, including judged candidates.
///
/// Session setup uses this entry point before resolving the judge gate. The
/// returned state keeps every winning candidate and its source bytes so a
/// later gate decision never needs to scan the filesystem again.
pub fn set_for_with_judged(
    input: &RuleBuildInput<'_>,
    data_root: &Path,
    workspace: &Path,
    cfg: &RulesConfig,
    include_judged: bool,
) -> RuleSet {
    let (winners, problems) = discover(input, data_root, workspace, cfg, include_judged);
    let state = Arc::new(BuildState::new(winners, problems, cfg.watch));
    view_for_agent(state, input.agent)
}

/// Builds the admitted user-file rules for this agent without compiling
/// plugin or project matchers. Discovery reads the current files on each call.
///
/// User winners precede all other candidates in the shared admission walk.
/// Later candidates cannot change their budgets, scopes, or precedence.
pub fn user_set_for(
    input: &RuleBuildInput<'_>,
    data_root: &Path,
    workspace: &Path,
    cfg: &RulesConfig,
) -> RuleSet {
    let (mut winners, problems) = discover(
        input,
        data_root,
        workspace,
        cfg,
        cfg.judge != dal_core::JudgeMode::Off,
    );
    winners.retain(|candidate| matches!(candidate.rule.origin, Origin::User(_)));
    let state = Arc::new(BuildState::new(winners, problems, cfg.watch));
    view_for_agent(state, input.agent)
}

fn discover(
    input: &RuleBuildInput<'_>,
    data_root: &Path,
    workspace: &Path,
    cfg: &RulesConfig,
    include_judged: bool,
) -> (Vec<snapshot::RuleCandidate>, Vec<Problem>) {
    let records = input.records;
    let plugin_rules = input.plugin_rules;
    let known_tools = input.known_tools;
    let mut builder = BuildContext {
        known_tools,
        include_judged,
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
                            .as_ref()
                            .map(|site| {
                                (
                                    site.path.as_os_str().as_encoded_bytes(),
                                    site.line,
                                    site.col,
                                )
                            })
                            .cmp(&right.site.as_ref().map(|site| {
                                (
                                    site.path.as_os_str().as_encoded_bytes(),
                                    site.line,
                                    site.col,
                                )
                            }))
                    })
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
    (builder.winners, builder.problems)
}

/// Builds another agent's view from the retained snapshot without filesystem
/// access. Agent filtering is repeated before the name-ordered bucket walk.
pub fn for_agent(set: &RuleSet, agent: &str) -> RuleSet {
    view_for_agent(Arc::clone(&set.state), agent)
}

/// Removes judged rules from this view without rebuilding or reading files.
///
/// The retained candidates, diagnostics, compiled matchers, and source bytes
/// remain shared with the original set.
pub fn without_judged(set: &RuleSet) -> RuleSet {
    view::view_for_agent_without_judged(Arc::clone(&set.state), &set.agent)
}
