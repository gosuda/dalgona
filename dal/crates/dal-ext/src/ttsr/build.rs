//! Deterministic rule discovery, precedence, compilation, and bucket selection.
//!
//! A [`RuleSet`] is the immutable snapshot shared with a session and its child
//! agents. The builder reads only the product data root and workspace supplied
//! by its caller; loaded plugins are an explicit snapshot, never process-global
//! state.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dal_core::{JudgeMode, RulesConfig};

use super::matcher;
pub use super::matcher::Compiled;
use super::record::{self, RecordSource};
use super::rulefile::{self, Value};
use super::scope;
use super::value::{Name, Origin, Problem, ProblemKind, Rule, Severity};

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

/// Builds a rule snapshot for the supplied loaded-plugin and record snapshots.
///
/// `loaded_plugins` supplies the names of plugins whose `rules/` directories
/// are eligible for discovery, including plugins that have no `dal.rule`
/// records. `records` carries each record's registering plugin explicitly.
/// `known_tools` is the product's loaded tool-name snapshot; unknown named
/// tools receive a Note while their scope remains intact.
/// The product-specific `data_root` is supplied by the caller; this function
/// never consults an ambient registry or reads another product's root.
///
/// Discovery order is user files, each plugin's records then files in plugin
/// name order, and project files. The returned view is initially filtered for
/// `agent`; the private candidate snapshot retains all matching-agent
/// alternatives for [`for_agent`].
pub fn set_for(
    records: &[RecordSource],
    loaded_plugins: &[dal_core::ext::Name],
    known_tools: &[&str],
    data_root: &Path,
    workspace: &Path,
    cfg: &RulesConfig,
    agent: &str,
) -> RuleSet {
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

    let loaded_plugin_names: HashSet<&str> = loaded_plugins
        .iter()
        .map(dal_core::ext::Name::as_str)
        .collect();
    let mut plugin_names = BTreeSet::new();
    for plugin in loaded_plugins {
        plugin_names.insert(plugin.as_str().to_owned());
    }
    let mut records_by_plugin: HashMap<&str, Vec<&RecordSource>> = HashMap::new();
    for source in records {
        plugin_names.insert(source.plugin.as_str().to_owned());
        records_by_plugin
            .entry(source.plugin.as_str())
            .or_default()
            .push(source);
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

        if loaded_plugin_names.contains(plugin.as_str()) {
            let plugin_dir = data_root.join("plugins").join(plugin).join("rules");
            add_rule_directory(&plugin_dir, DirectoryOrigin::Plugin(plugin), &mut builder);
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

impl RuleSet {
    /// Returns the concatenated always-apply bodies, separated by one blank
    /// line. Rules are already in deterministic name order.
    #[must_use]
    pub fn always_text(&self) -> String {
        let mut text = String::new();
        for rule in &self.always {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&rule.body);
        }
        text
    }

    /// Returns one rulebook entry per line, preserving authored glob strings.
    /// A line break in a description is rendered as one space.
    #[must_use]
    pub fn rulebook_text(&self) -> String {
        let mut lines = Vec::with_capacity(self.rulebook.len());
        for rule in &self.rulebook {
            let mut line = format!("- {}", rule.name.as_str());
            if let Some(globs) = self.state.rulebook_globs.get(&rule.name)
                && !globs.is_empty()
            {
                line.push_str(" (");
                line.push_str(&globs.join(", "));
                line.push(')');
            }
            line.push_str(": ");
            line.push_str(
                &rule
                    .description
                    .as_deref()
                    .unwrap_or_default()
                    .replace(['\r', '\n'], " "),
            );
            lines.push(line);
        }
        lines.join("\n")
    }

    /// Returns the original bytes of a winning rule file, if this rule came
    /// from disk. Record rules have no file source and return `None`.
    #[must_use]
    pub fn source_bytes(&self, name: &str) -> Option<&[u8]> {
        self.state.source_bytes.get(name).map(Arc::<[u8]>::as_ref)
    }

    /// Returns the precompiled matchers for a winning rule name.
    ///
    /// Stream consumers reuse these matchers rather than compiling a regular
    /// expression for each response. A valid rule name with no usable matcher
    /// returns an empty slice; an unknown name returns `None`.
    #[must_use]
    pub fn compiled_conditions(&self, name: &str) -> Option<&[Arc<Compiled>]> {
        let index = *self.state.candidate_by_name.get(name)?;
        Some(self.state.candidates[index].compiled.as_slice())
    }
}

#[derive(Debug)]
struct BuildState {
    candidates: Vec<Candidate>,
    candidate_by_name: HashMap<Name, usize>,
    base_problems: Vec<Problem>,
    watch: bool,
    source_bytes: HashMap<Name, Arc<[u8]>>,
    rulebook_globs: HashMap<Name, Vec<String>>,
}

impl BuildState {
    fn new(winners: Vec<RuleCandidate>, base_problems: Vec<Problem>, watch: bool) -> Self {
        let mut candidates = Vec::with_capacity(winners.len());
        let mut candidate_by_name = HashMap::with_capacity(winners.len());
        let mut source_bytes = HashMap::new();
        let mut rulebook_globs = HashMap::new();

        for winner in winners {
            let RuleCandidate {
                rule,
                rulebook_globs: globs,
                shorthand_fallback,
                source_bytes: bytes,
            } = winner;
            let name = rule.name.clone();
            if let Some(bytes) = bytes {
                source_bytes.insert(name.clone(), bytes);
            }
            rulebook_globs.insert(name.clone(), globs);
            candidate_by_name.insert(name, candidates.len());
            candidates.push(Candidate::compile(rule, shorthand_fallback, watch));
        }

        Self {
            candidates,
            candidate_by_name,
            base_problems,
            watch,
            source_bytes,
            rulebook_globs,
        }
    }
}

#[derive(Debug)]
struct Candidate {
    rule: Arc<Rule>,
    compiled: Vec<Arc<Compiled>>,
    compile_skips: Vec<Problem>,
    empty_notes: Vec<Problem>,
}

impl Candidate {
    fn compile(rule: Rule, shorthand_fallback: bool, watch: bool) -> Self {
        let mut compiled = Vec::new();
        let mut compile_skips = Vec::new();
        let mut empty_notes = Vec::new();
        if watch && rule.scope.reaches_any() {
            for condition in &rule.conditions {
                match matcher::compile(&condition.src, condition.index) {
                    Ok(condition_matcher) => {
                        if condition_matcher.matches_empty() && !shorthand_fallback {
                            empty_notes.push(Problem {
                                origin: rule.origin.clone(),
                                kind: ProblemKind::SetNote,
                                reason: format!(
                                    "condition {} \"{}\" matches empty text, so it fires on the first output of every stream it watches.",
                                    condition.index + 1,
                                    condition.src
                                ),
                                consequence: String::new(),
                                severity: Severity::Note,
                            });
                        }
                        compiled.push(Arc::new(condition_matcher));
                    }
                    Err(skip) => compile_skips.push(skip.problem(rule.origin.clone())),
                }
            }
        }

        Self {
            rule: Arc::new(rule),
            compiled,
            compile_skips,
            empty_notes,
        }
    }
}

#[derive(Debug)]
struct RuleCandidate {
    rule: Rule,
    rulebook_globs: Vec<String>,
    shorthand_fallback: bool,
    source_bytes: Option<Arc<[u8]>>,
}

struct BuildContext<'cfg, 'tools, 'tool> {
    cfg: &'cfg RulesConfig,
    known_tools: &'tools [&'tool str],
    disabled: BTreeSet<&'cfg str>,
    winners: Vec<RuleCandidate>,
    by_name: HashMap<Name, usize>,
    problems: Vec<Problem>,
}
#[derive(Clone, Copy)]
enum DirectoryOrigin<'a> {
    User,
    Plugin(&'a str),
    Project,
}

impl DirectoryOrigin<'_> {
    fn for_directory(self, path: &Path) -> Origin {
        match self {
            Self::User => Origin::User(path.to_path_buf()),
            Self::Plugin(plugin) => Origin::Plugin {
                plugin: plugin.into(),
                path: None,
            },
            Self::Project => Origin::Project(path.to_path_buf()),
        }
    }

    fn for_file(self, path: &Path) -> Origin {
        match self {
            Self::User => Origin::User(path.to_path_buf()),
            Self::Plugin(plugin) => Origin::Plugin {
                plugin: plugin.into(),
                path: Some(path.to_path_buf()),
            },
            Self::Project => Origin::Project(path.to_path_buf()),
        }
    }
}

fn add_rule_directory(
    directory: &Path,
    origin: DirectoryOrigin<'_>,
    builder: &mut BuildContext<'_, '_, '_>,
) {
    let paths = rule_files(directory, origin, &mut builder.problems);
    for path in paths {
        let file_origin = origin.for_file(&path);
        let Some(bytes) = read_rule_file(&path, &file_origin, &mut builder.problems) else {
            continue;
        };
        let name = file_rule_name(&path);
        match rulefile::parse_rulefile(
            &name,
            file_origin.clone(),
            Some(builder.known_tools),
            &bytes,
        ) {
            Ok((rule, notes)) => {
                let (shorthand_fallback, rulebook_globs) = file_metadata(&bytes);
                builder.accept_rule(
                    RuleCandidate {
                        rule,
                        rulebook_globs,
                        shorthand_fallback,
                        source_bytes: Some(Arc::from(bytes)),
                    },
                    notes,
                );
            }
            Err(rejections) => builder.problems.extend(rejections),
        }
    }
}

fn add_record(source: &RecordSource, builder: &mut BuildContext<'_, '_, '_>) {
    let origin = Origin::Record {
        plugin: source.plugin.as_str().into(),
    };
    match record::validate_record(source, Some(builder.known_tools)) {
        Ok((rule, notes)) => {
            let patterns: Vec<&str> = source.record.patterns.iter().map(AsRef::as_ref).collect();
            let shorthand_fallback = !patterns.is_empty()
                && patterns
                    .iter()
                    .all(|pattern| matcher::is_glob_shorthand(pattern));
            let rulebook_globs = source
                .record
                .globs
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(ToString::to_string)
                .collect();
            builder.accept_rule(
                RuleCandidate {
                    rule,
                    rulebook_globs,
                    shorthand_fallback,
                    source_bytes: None,
                },
                notes,
            );
        }
        Err(error) => builder.problems.push(Problem {
            origin,
            kind: ProblemKind::Rule,
            reason: error.reason().to_owned(),
            consequence: SKIPPED.to_owned(),
            severity: Severity::Skipped,
        }),
    }
}

impl BuildContext<'_, '_, '_> {
    fn accept_rule(&mut self, candidate: RuleCandidate, notes: Vec<Problem>) {
        let rule = &candidate.rule;
        if self.disabled.contains(rule.name.as_str()) {
            return;
        }
        if !rule.enabled {
            self.problems.extend(notes);
            self.problems.push(Problem {
                origin: rule.origin.clone(),
                kind: ProblemKind::SetNote,
                reason: "\"enabled: false\" turns this rule off".to_owned(),
                consequence: String::new(),
                severity: Severity::Note,
            });
            return;
        }
        if self.cfg.judge == JudgeMode::Off && rule.judge.is_some() {
            return;
        }

        self.problems.extend(notes);
        let name = rule.name.clone();
        let Some(&existing_index) = self.by_name.get(&name) else {
            self.by_name.insert(name, self.winners.len());
            self.winners.push(candidate);
            return;
        };

        let new_wins = origin_preference(
            &candidate.rule.origin,
            &self.winners[existing_index].rule.origin,
        )
        .is_lt();
        if new_wins {
            let loser = std::mem::replace(&mut self.winners[existing_index], candidate);
            self.problems.push(duplicate_problem(
                &loser.rule,
                &self.winners[existing_index].rule,
            ));
        } else {
            self.problems.push(duplicate_problem(
                &candidate.rule,
                &self.winners[existing_index].rule,
            ));
        }
    }
}

fn origin_preference(left: &Origin, right: &Origin) -> std::cmp::Ordering {
    let rank = left.rank().cmp(&right.rank());
    if !rank.is_eq() {
        return rank;
    }

    match (plugin_name(left), plugin_name(right)) {
        (Some(left_plugin), Some(right_plugin)) => {
            let plugin_order = left_plugin.cmp(right_plugin);
            if !plugin_order.is_eq() {
                return plugin_order;
            }
            match (left, right) {
                (Origin::Record { .. }, Origin::Plugin { .. }) => std::cmp::Ordering::Less,
                (Origin::Plugin { .. }, Origin::Record { .. }) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            }
        }
        _ => std::cmp::Ordering::Equal,
    }
}

fn plugin_name(origin: &Origin) -> Option<&str> {
    match origin {
        Origin::Plugin { plugin, .. } | Origin::Record { plugin } => Some(plugin),
        Origin::User(_) | Origin::Project(_) => None,
    }
}

fn duplicate_problem(loser: &Rule, winner: &Rule) -> Problem {
    let plugin_replaced_by_user = matches!(&winner.origin, Origin::User(_))
        && matches!(&loser.origin, Origin::Plugin { .. } | Origin::Record { .. });
    let (kind, reason, consequence, severity) = if plugin_replaced_by_user {
        let plugin = plugin_name(&loser.origin).unwrap_or_default();
        (
            ProblemKind::SetNote,
            format!(
                "rule \"{}\" from plugin {plugin} is replaced by {}",
                loser.name,
                winner.origin.source_label()
            ),
            String::new(),
            Severity::Note,
        )
    } else {
        (
            ProblemKind::Set,
            format!(
                "rule \"{}\" is also defined by {}, which takes precedence",
                loser.name,
                winner.origin.source_label()
            ),
            "dalgon skipped this one.".to_owned(),
            Severity::Skipped,
        )
    };
    Problem {
        origin: loser.origin.clone(),
        kind,
        reason,
        consequence,
        severity,
    }
}

fn rule_files(
    directory: &Path,
    origin: DirectoryOrigin<'_>,
    problems: &mut Vec<Problem>,
) -> Vec<PathBuf> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            problems.push(directory_problem(directory, origin, &error));
            return Vec::new();
        }
    };

    // Retain only the first 256 names while scanning. This bounds temporary
    // memory even when a directory contains many unrelated entries.
    let mut paths: Vec<PathBuf> = Vec::with_capacity(DIRECTORY_LIMIT);
    let mut exceeded = false;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                problems.push(directory_problem(directory, origin, &error));
                return Vec::new();
            }
        };
        let file_name = entry.file_name();
        let encoded_name = file_name.as_encoded_bytes();
        if encoded_name.first() == Some(&b'.') || !encoded_name.ends_with(b".md") {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                problems.push(directory_problem(directory, origin, &error));
                return Vec::new();
            }
        };
        let regular = if file_type.is_file() {
            true
        } else if file_type.is_symlink() {
            match fs::metadata(entry.path()) {
                Ok(metadata) => metadata.is_file(),
                // An unavailable link is retained so the read stage reports
                // its path and OS error, and so it cannot claim a rule name.
                Err(_) => true,
            }
        } else {
            false
        };
        if !regular {
            continue;
        }

        let insertion = paths
            .binary_search_by(|path| path_name_bytes(path).cmp(encoded_name))
            .unwrap_or_else(|index| index);
        if paths.len() == DIRECTORY_LIMIT && insertion == DIRECTORY_LIMIT {
            exceeded = true;
            continue;
        }
        paths.insert(insertion, entry.path());
        if paths.len() > DIRECTORY_LIMIT {
            paths.pop();
            exceeded = true;
        }
    }

    if exceeded {
        problems.push(Problem {
            origin: origin.for_directory(directory),
            kind: ProblemKind::Directory,
            reason: "the directory holds more than 256 rule files".to_owned(),
            consequence: "dalgon read the first 256 by name.".to_owned(),
            severity: Severity::Skipped,
        });
    }
    paths
}

fn path_name_bytes(path: &Path) -> &[u8] {
    path.file_name().map_or(&[], |name| name.as_encoded_bytes())
}

fn read_rule_file(path: &Path, origin: &Origin, problems: &mut Vec<Problem>) -> Option<Vec<u8>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            problems.push(file_problem(
                origin,
                format!("cannot read the file: {error}"),
            ));
            return None;
        }
    };
    if metadata.len() > 65_536 {
        problems.push(file_problem(origin, "the file is larger than 65536 bytes"));
        return None;
    }

    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            problems.push(file_problem(
                origin,
                format!("cannot read the file: {error}"),
            ));
            return None;
        }
    };
    let mut bytes = Vec::new();
    let mut limited = file.take(65_537);
    if let Err(error) = limited.read_to_end(&mut bytes) {
        problems.push(file_problem(
            origin,
            format!("cannot read the file: {error}"),
        ));
        return None;
    }
    if bytes.len() > FILE_LIMIT {
        problems.push(file_problem(origin, "the file is larger than 65536 bytes"));
        return None;
    }
    Some(bytes)
}

fn file_problem(origin: &Origin, reason: impl Into<String>) -> Problem {
    Problem {
        origin: origin.clone(),
        kind: ProblemKind::File,
        reason: reason.into(),
        consequence: SKIPPED.to_owned(),
        severity: Severity::Skipped,
    }
}

fn directory_problem(
    directory: &Path,
    origin: DirectoryOrigin<'_>,
    error: &std::io::Error,
) -> Problem {
    Problem {
        origin: origin.for_directory(directory),
        kind: ProblemKind::Directory,
        reason: format!("cannot read the directory: {error}"),
        consequence: SKIPPED.to_owned(),
        severity: Severity::Skipped,
    }
}

fn file_rule_name(path: &Path) -> String {
    let Some(name) = path.file_name() else {
        return String::new();
    };
    let bytes = name
        .as_encoded_bytes()
        .strip_suffix(b".md")
        .unwrap_or(name.as_encoded_bytes());
    String::from_utf8_lossy(bytes).into_owned()
}

fn file_metadata(bytes: &[u8]) -> (bool, Vec<String>) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (false, Vec::new());
    };
    let (front, _, _) = rulefile::split(text);
    let conditions: Vec<&str> = match front.get("condition").map(|entry| &entry.value) {
        Some(Value::Str(value)) => vec![value],
        Some(Value::List(values)) => values.iter().map(String::as_str).collect(),
        _ => Vec::new(),
    };
    let shorthand_fallback = !conditions.is_empty()
        && conditions
            .iter()
            .all(|condition| matcher::is_glob_shorthand(condition));
    let rulebook_globs = match front.get("globs").map(|entry| &entry.value) {
        Some(Value::Str(value)) => vec![value.clone()],
        Some(Value::List(values)) => values.clone(),
        _ => Vec::new(),
    };
    (shorthand_fallback, rulebook_globs)
}

fn scope_problem(rule: &Rule) -> Problem {
    Problem {
        origin: rule.origin.clone(),
        kind: ProblemKind::Set,
        reason: "the scope reaches no output".to_owned(),
        consequence: "dalgon does not watch this rule.".to_owned(),
        severity: Severity::Skipped,
    }
}

fn watch_problem(rule: &Rule) -> Problem {
    Problem {
        origin: rule.origin.clone(),
        kind: ProblemKind::SetNote,
        reason: "rules.watch is false, so dalgon does not watch this rule".to_owned(),
        consequence: String::new(),
        severity: Severity::Note,
    }
}

fn no_usable_condition_problem(rule: &Rule) -> Problem {
    Problem {
        origin: rule.origin.clone(),
        kind: ProblemKind::Set,
        reason: "the rule has no usable condition, no \"alwaysApply: true\", and no description"
            .to_owned(),
        consequence: SKIPPED.to_owned(),
        severity: Severity::Skipped,
    }
}

fn set_note(rule: &Rule, reason: impl Into<String>) -> Problem {
    Problem {
        origin: rule.origin.clone(),
        kind: ProblemKind::SetNote,
        reason: reason.into(),
        consequence: String::new(),
        severity: Severity::Note,
    }
}

fn view_for_agent(state: Arc<BuildState>, agent: &str) -> RuleSet {
    let mut problems = state.base_problems.clone();
    let mut selected: Vec<&Candidate> = state
        .candidates
        .iter()
        .filter(|candidate| {
            candidate
                .rule
                .agents
                .as_ref()
                .is_none_or(|agents| agents.is_match(agent))
        })
        .collect();
    selected.sort_by(|left, right| left.rule.name.cmp(&right.rule.name));

    let mut stream = Vec::new();
    let mut always = Vec::new();
    let mut rulebook = Vec::new();
    let mut stream_conditions: usize = 0;
    let mut always_bytes: usize = 0;

    for candidate in selected {
        let rule = candidate.rule.as_ref();
        let has_conditions = !rule.conditions.is_empty();
        let stream_candidate = if !has_conditions {
            false
        } else if !state.watch {
            problems.push(watch_problem(rule));
            false
        } else if !scope::reaches_output(&rule.scope) {
            problems.push(scope_problem(rule));
            false
        } else {
            problems.extend(candidate.compile_skips.iter().cloned());
            if candidate.compiled.is_empty() {
                false
            } else if candidate.compiled.len() > STREAM_CONDITION_LIMIT - stream_conditions {
                problems.push(Problem {
                    origin: rule.origin.clone(),
                    kind: ProblemKind::Set,
                    reason: "the stream rules already hold 256 conditions".to_owned(),
                    consequence: "dalgon does not watch this rule.".to_owned(),
                    severity: Severity::Skipped,
                });
                false
            } else {
                stream_conditions += candidate.compiled.len();
                problems.extend(candidate.empty_notes.iter().cloned());
                if rule.always_apply {
                    problems.push(set_note(
                        rule,
                        "\"alwaysApply: true\" has no effect because the rule has a condition",
                    ));
                }
                if rule.globs.is_some() {
                    problems.push(set_note(
                        rule,
                        "globs limit this rule to tool calls on matching paths, so it never fires on text or thinking",
                    ));
                }
                stream.push(Arc::clone(&candidate.rule));
                true
            }
        };
        if stream_candidate {
            continue;
        }

        if rule.always_apply {
            let separator_bytes = if always.is_empty() { 0 } else { 2 };
            let requested = rule.body.len().saturating_add(separator_bytes);
            if rule.body.len() > ALWAYS_BODY_LIMIT
                || always_bytes.saturating_add(requested) > ALWAYS_TOTAL_LIMIT
            {
                problems.push(set_note(
                    rule,
                    "the always-apply budget is exhausted; this rule is listed in the rulebook instead.",
                ));
            } else {
                always_bytes += requested;
                always.push(Arc::clone(&candidate.rule));
                continue;
            }
        }
        if rule.description.is_some() {
            rulebook.push(Arc::clone(&candidate.rule));
        } else {
            problems.push(no_usable_condition_problem(rule));
        }
    }

    RuleSet {
        stream,
        always,
        rulebook,
        problems,
        state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::super::ToolScope;
    use dal_core::ext::{InterruptMode, Name as PluginName, RepeatMode, RuleRecord, Scope, Site};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        #[expect(
            clippy::create_dir,
            reason = "exclusive test directory creation must reject collisions"
        )]
        fn new() -> Self {
            loop {
                let path = std::env::temp_dir().join(format!(
                    "dal-ext-ttsr-build-{}-{}",
                    std::process::id(),
                    NEXT_DIR.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("create temporary test directory: {error}"),
                }
            }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn core_name(value: &str) -> PluginName {
        PluginName::parse(value).expect("valid test plugin name")
    }

    fn write_rule(directory: &Path, name: &str, front: &str, body: &str) -> PathBuf {
        fs::create_dir_all(directory).expect("create rule directory");
        let path = directory.join(format!("{name}.md"));
        let contents = format!("---\n{front}---\n{body}\n");
        fs::write(&path, contents).expect("write rule file");
        path
    }

    fn set(data_root: &Path, workspace: &Path, cfg: &RulesConfig, agent: &str) -> RuleSet {
        set_for(&[], &[], &[], data_root, workspace, cfg, agent)
    }

    fn record_source(plugin: &PluginName, name: &str, text: &str) -> RecordSource {
        RecordSource {
            plugin: plugin.clone(),
            site: Site {
                path: PathBuf::from(format!("{}/rules.star", plugin.as_str())),
                line: 1,
                col: 1,
            },
            record: RuleRecord {
                name: PluginName::parse(name).expect("valid record rule name"),
                patterns: vec!["sleep".into()],
                text: text.into(),
                judge: None,
                scope: None,
                globs: None,
                agents: None,
                mode: Some(InterruptMode::Always),
                repeat_mode: Some(RepeatMode::Once),
                repeat_gap: Some(1),
                always_apply: false,
                report: false,
                enabled: true,
            },
        }
    }

    fn rule_names(rules: &[Arc<Rule>]) -> Vec<String> {
        rules
            .iter()
            .map(|rule| rule.name.as_str().to_owned())
            .collect()
    }

    #[test]
    fn name_precedence_roots() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        let user = write_rule(
            &data.join("rules"),
            "no-sleep",
            "condition: sleep\n",
            "User.",
        );
        write_rule(
            &data.join("plugins/p/rules"),
            "no-sleep",
            "condition: sleep\n",
            "Plugin.",
        );
        write_rule(
            &workspace.join(".dal/rules"),
            "no-sleep",
            "condition: sleep\n",
            "Project.",
        );

        let set = set_for(
            &[],
            &[core_name("p")],
            &[],
            &data,
            &workspace,
            &RulesConfig::default(),
            "main",
        );
        assert_eq!(set.stream.len(), 1);
        assert_eq!(set.stream[0].body, "User.");
        let user_source = user.display().to_string();
        assert!(set.problems.iter().any(|problem| {
            problem.reason
                == format!("rule \"no-sleep\" from plugin p is replaced by {user_source}")
        }));
        assert!(set.problems.iter().any(|problem| {
            problem.reason
                == format!(
                    "rule \"no-sleep\" is also defined by {user_source}, which takes precedence"
                )
                && problem.consequence == "dalgon skipped this one."
        }));
    }

    #[test]
    fn plugin_code_and_name_precedence_are_deterministic() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        write_rule(
            &data.join("plugins/a/rules"),
            "shared",
            "condition: sleep\n",
            "A file.",
        );
        write_rule(
            &data.join("plugins/b/rules"),
            "shared",
            "condition: sleep\n",
            "B file.",
        );
        let plugin_a = core_name("a");
        let plugin_b = core_name("b");
        let records = [
            record_source(&plugin_b, "shared", "B code."),
            record_source(&plugin_a, "shared", "A code."),
        ];
        let set = set_for(
            &records,
            &[plugin_b, plugin_a],
            &[],
            &data,
            &workspace,
            &RulesConfig::default(),
            "main",
        );
        assert_eq!(set.stream.len(), 1);
        assert_eq!(set.stream[0].body, "A code.");
        assert_eq!(set.stream[0].origin, Origin::Record { plugin: "a".into() });
        assert!(
            set.problems
                .iter()
                .filter(|problem| problem.kind == ProblemKind::Set)
                .all(|problem| problem.reason
                    == "rule \"shared\" is also defined by plugin:a, which takes precedence")
        );
        assert_eq!(
            set.problems
                .iter()
                .filter(|problem| problem.kind == ProblemKind::Set)
                .count(),
            3
        );
    }

    #[test]
    fn invalid_high_priority_file_does_not_claim_name() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        write_rule(&data.join("rules"), "shared", "condition: sleep\n", "   ");
        let project = write_rule(
            &workspace.join(".dal/rules"),
            "shared",
            "condition: sleep\n",
            "Project rule.",
        );

        let set = set(&data, &workspace, &RulesConfig::default(), "main");
        assert_eq!(set.stream.len(), 1);
        assert_eq!(set.stream[0].origin, Origin::Project(project));
        assert!(
            set.problems
                .iter()
                .any(|problem| problem.reason == "the body is empty")
        );
        assert!(
            !set.problems
                .iter()
                .any(|problem| problem.reason.contains("takes precedence"))
        );
    }

    #[test]
    fn disabled_rule_does_not_claim_name() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        write_rule(
            &data.join("rules"),
            "shared",
            "enabled: false\ncondition: sleep\n",
            "Disabled.",
        );
        let project = write_rule(
            &workspace.join(".dal/rules"),
            "shared",
            "condition: sleep\n",
            "Project rule.",
        );

        let set = set(&data, &workspace, &RulesConfig::default(), "main");
        assert_eq!(set.stream.len(), 1);
        assert_eq!(set.stream[0].origin, Origin::Project(project));
        assert!(set.problems.iter().any(|problem| {
            problem.reason == "\"enabled: false\" turns this rule off"
                && problem.severity == Severity::Note
        }));
        assert!(
            !set.problems
                .iter()
                .any(|problem| problem.reason.contains("takes precedence"))
        );
    }

    #[test]
    fn unknown_tool_note_keeps_scope() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        let path = write_rule(
            &data.join("rules"),
            "unknown-tool",
            "scope: tool:patch, tool:missing\ncondition: sleep\n",
            "Keep the declared scope.",
        );
        let known_tools = ["patch"];

        let set = set_for(
            &[],
            &[],
            &known_tools,
            &data,
            &workspace,
            &RulesConfig::default(),
            "main",
        );
        assert_eq!(set.stream.len(), 1);
        let ToolScope::Tools(tools) = &set.stream[0].scope.tools else {
            panic!("explicit tool scope must remain a tool list");
        };
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.tool.as_ref())
                .collect::<Vec<_>>(),
            vec!["patch", "missing"]
        );
        assert!(tools[0].available);
        assert!(!tools[1].available);
        let note = set
            .problems
            .iter()
            .find(|problem| {
                problem.reason == "the scope names the tool \"missing\", which dalgon does not have"
            })
            .expect("unknown tool gets one note");
        assert_eq!(note.origin, Origin::User(path));
        assert_eq!(note.severity, Severity::Note);
    }

    #[test]
    fn judge_off_drops_records_before_precedence() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        let plugin = core_name("p");
        let mut judged = record_source(&plugin, "shared", "Judged body.");
        judged.record.judge = Some("Is this a sleep call?".into());
        let project = write_rule(
            &workspace.join(".dal/rules"),
            "shared",
            "condition: sleep\n",
            "Project rule.",
        );
        let cfg = RulesConfig {
            judge: JudgeMode::Off,
            ..RulesConfig::default()
        };

        let set = set_for(
            &[judged],
            std::slice::from_ref(&plugin),
            &[],
            &data,
            &workspace,
            &cfg,
            "main",
        );
        assert_eq!(set.stream.len(), 1);
        assert_eq!(set.stream[0].origin, Origin::Project(project));
        assert!(
            !set.problems
                .iter()
                .any(|problem| problem.reason.contains("takes precedence"))
        );
    }

    #[test]
    fn always_budgets_fall_through_to_rulebook() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        let user = data.join("rules");
        let body = "x".repeat(9_998);
        for name in ["a", "b", "c", "d"] {
            write_rule(
                &user,
                name,
                "alwaysApply: true\ndescription: retained\n",
                &body,
            );
        }
        write_rule(
            &user,
            "e",
            "alwaysApply: true\ndescription: total overflow\n",
            "x",
        );
        write_rule(
            &user,
            "z",
            "alwaysApply: true\ndescription: per-rule overflow\n",
            &"z".repeat(ALWAYS_BODY_LIMIT + 1),
        );

        let set = set(&data, &workspace, &RulesConfig::default(), "main");
        assert_eq!(
            rule_names(&set.always),
            vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned()
            ]
        );
        assert_eq!(
            rule_names(&set.rulebook),
            vec!["e".to_owned(), "z".to_owned()]
        );
        assert_eq!(
            set.problems
                .iter()
                .filter(|problem| problem
                    .reason
                    .starts_with("the always-apply budget is exhausted"))
                .count(),
            2
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_symlink_claims_no_name() {
        use std::os::unix::fs::symlink;

        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        let user_dir = data.join("rules");
        fs::create_dir_all(&user_dir).expect("create user rules directory");
        symlink(user_dir.join("missing-target"), user_dir.join("shared.md"))
            .expect("create dangling rule symlink");
        let project = write_rule(
            &workspace.join(".dal/rules"),
            "shared",
            "condition: sleep\n",
            "Project rule.",
        );

        let set = set(&data, &workspace, &RulesConfig::default(), "main");
        assert_eq!(set.stream.len(), 1);
        assert_eq!(set.stream[0].origin, Origin::Project(project));
        assert!(set.problems.iter().any(|problem| {
            problem.kind == ProblemKind::File && problem.reason.starts_with("cannot read the file:")
        }));
    }

    #[test]
    fn oversized_file_is_skipped_without_claiming_name() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        let user_dir = data.join("rules");
        fs::create_dir_all(&user_dir).expect("create user rules directory");
        fs::write(user_dir.join("shared.md"), vec![b'x'; FILE_LIMIT + 1])
            .expect("write oversized file");
        let project = write_rule(
            &workspace.join(".dal/rules"),
            "shared",
            "condition: sleep\n",
            "Project rule.",
        );

        let set = set(&data, &workspace, &RulesConfig::default(), "main");
        assert_eq!(set.stream.len(), 1);
        assert_eq!(set.stream[0].origin, Origin::Project(project));
        assert!(set.problems.iter().any(|problem| {
            problem.reason == "the file is larger than 65536 bytes"
                && problem.kind == ProblemKind::File
        }));
    }

    #[test]
    fn rulebook_and_always_texts_are_stable() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        write_rule(
            &data.join("rules"),
            "a",
            "alwaysApply: true\n",
            "Always body.",
        );
        write_rule(
            &data.join("rules"),
            "b",
            "description: \"first\\nsecond\"\nglobs: [\"*.ml\", \"*.mli\"]\n",
            "Rulebook body.",
        );

        let set = set(&data, &workspace, &RulesConfig::default(), "main");
        assert_eq!(set.always_text(), "Always body.");
        assert_eq!(set.rulebook_text(), "- b (*.ml, *.mli): first second");
    }

    #[test]
    fn glob_shorthand_fallback_has_no_empty_text_note() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        write_rule(
            &data.join("rules"),
            "glob-rule",
            "condition: *.ml\n",
            "Patch files only.",
        );

        let known_tools = ["patch"];
        let set = set_for(
            &[],
            &[],
            &known_tools,
            &data,
            &workspace,
            &RulesConfig::default(),
            "main",
        );
        assert_eq!(rule_names(&set.stream), vec!["glob-rule".to_owned()]);
        assert_eq!(
            set.compiled_conditions("glob-rule").map(<[_]>::len),
            Some(1)
        );
        assert!(!set.problems.iter().any(|problem| {
            problem.reason.starts_with("condition 1 ")
                && problem.reason.contains("matches empty text")
        }));
    }

    #[test]
    fn load_order_does_not_change_buckets_or_prompt_text() {
        let left = TestDir::new();
        let right = TestDir::new();
        let left_root = left.0.join("data/rules");
        let right_root = right.0.join("data/rules");
        let rules = [
            ("z", "description: zed\n", "z body"),
            ("a", "alwaysApply: true\n", "a body"),
            ("m", "description: em\n", "m body"),
        ];
        for (name, front, body) in rules {
            write_rule(&left_root, name, front, body);
        }
        for (name, front, body) in rules.into_iter().rev() {
            write_rule(&right_root, name, front, body);
        }

        let left_set = set(
            &left.0.join("data"),
            &left.0.join("workspace"),
            &RulesConfig::default(),
            "main",
        );
        let right_set = set(
            &right.0.join("data"),
            &right.0.join("workspace"),
            &RulesConfig::default(),
            "main",
        );
        assert_eq!(rule_names(&left_set.stream), rule_names(&right_set.stream));
        assert_eq!(rule_names(&left_set.always), rule_names(&right_set.always));
        assert_eq!(
            rule_names(&left_set.rulebook),
            rule_names(&right_set.rulebook)
        );
        assert_eq!(left_set.always_text(), right_set.always_text());
        assert_eq!(left_set.rulebook_text(), right_set.rulebook_text());
    }

    #[test]
    fn directory_cap_and_filters_are_applied() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        let user_dir = data.join("rules");
        write_rule(
            &user_dir,
            "a",
            "description: disabled\ncondition: x\n",
            "body",
        );
        write_rule(
            &user_dir,
            "r000",
            "description: too large\ncondition: x\n",
            "body",
        );
        fs::write(user_dir.join("r000.md"), vec![b'x'; FILE_LIMIT + 1])
            .expect("replace with oversized file");
        write_rule(
            &user_dir,
            "r001",
            "agents: [sub]\ndescription: child only\ncondition: x\n",
            "body",
        );
        for index in 2..=255 {
            write_rule(
                &user_dir,
                &format!("r{index:03}"),
                "description: listed\ncondition: x\n",
                "body",
            );
        }
        let cfg = RulesConfig {
            watch: false,
            disabled: vec!["a".into()],
            ..RulesConfig::default()
        };

        let parent = set(&data, &workspace, &cfg, "main");
        let child = for_agent(&parent, "sub");
        assert!(!parent.always.iter().any(|rule| rule.name.as_str() == "a"));
        assert!(!parent.rulebook.iter().any(|rule| rule.name.as_str() == "a"));
        assert!(
            child
                .rulebook
                .iter()
                .any(|rule| rule.name.as_str() == "r001")
        );
        assert!(
            parent.problems.iter().any(|problem| {
                problem.reason == "the directory holds more than 256 rule files"
            })
        );
        assert!(
            parent
                .problems
                .iter()
                .any(|problem| { problem.reason == "the file is larger than 65536 bytes" })
        );
        assert!(child.problems.iter().any(|problem| {
            problem.reason == "rules.watch is false, so dalgon does not watch this rule"
        }));
        assert!(
            !parent
                .problems
                .iter()
                .any(|problem| problem.origin.source_label().ends_with("/a.md"))
        );
    }

    #[test]
    fn stream_condition_cap_is_applied_in_name_order() {
        let temp = TestDir::new();
        let plugin = core_name("p");
        let records: Vec<RecordSource> = (0..=STREAM_CONDITION_LIMIT)
            .map(|index| RecordSource {
                plugin: plugin.clone(),
                site: Site {
                    path: PathBuf::from("p/rules.star"),
                    line: 1,
                    col: 1,
                },
                record: RuleRecord {
                    name: dal_core::ext::Name::parse(&format!("r{index:03}"))
                        .expect("valid rule name"),
                    patterns: vec!["x".into()],
                    text: "Rule body.".into(),
                    judge: None,
                    scope: Some(Scope {
                        text: true,
                        thinking: false,
                        tool: false,
                        named_tools: Vec::new(),
                    }),
                    globs: None,
                    agents: None,
                    mode: Some(InterruptMode::Always),
                    repeat_mode: Some(RepeatMode::Once),
                    repeat_gap: Some(1),
                    always_apply: false,
                    report: false,
                    enabled: true,
                },
            })
            .collect();
        let set = set_for(
            &records,
            std::slice::from_ref(&plugin),
            &[],
            &temp.0.join("data"),
            &temp.0.join("workspace"),
            &RulesConfig::default(),
            "main",
        );
        assert_eq!(set.stream.len(), STREAM_CONDITION_LIMIT);
        assert_eq!(set.compiled_conditions("r000").map(<[_]>::len), Some(1));
        assert!(set.problems.iter().any(|problem| {
            problem.reason == "the stream rules already hold 256 conditions"
                && problem.origin.source_label() == "plugin:p"
        }));
    }

    #[test]
    fn child_filter_uses_retained_bytes_after_files_disappear() {
        let temp = TestDir::new();
        let data = temp.0.join("data");
        let workspace = temp.0.join("workspace");
        let child_path = write_rule(
            &data.join("rules"),
            "child-rule",
            "agents: [sub]\ncondition: child-marker\n",
            "Child body.",
        );
        let parent = set(&data, &workspace, &RulesConfig::default(), "main");
        assert!(parent.stream.is_empty());
        assert_eq!(
            parent.source_bytes("child-rule"),
            Some(b"---\nagents: [sub]\ncondition: child-marker\n---\nChild body.\n".as_slice())
        );
        fs::remove_file(child_path).expect("remove source file");
        fs::remove_dir_all(data.join("rules")).expect("remove source directory");

        let child = for_agent(&parent, "sub");
        assert_eq!(rule_names(&child.stream), vec!["child-rule".to_owned()]);
        assert_eq!(
            child.source_bytes("child-rule"),
            parent.source_bytes("child-rule")
        );
    }
}
