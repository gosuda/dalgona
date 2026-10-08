//! Rule loading: directories, plugin files, and records into winners.
//!
//! The loader fills the [`BuildContext`] winners in precedence order; the
//! snapshot compiler and the bucket walk consume them from there.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dal_core::{JudgeMode, RulesConfig, ext::RuleFile};

use super::super::matcher;
use super::super::record::{RecordSource, validate_record};
use super::super::rulefile::{self, Value};
use super::super::scope;
use super::super::value::{Name, Origin, Problem, ProblemKind, Severity};
use super::snapshot::RuleCandidate;
use super::view::duplicate_problem;
use super::{DIRECTORY_LIMIT, FILE_LIMIT, SKIPPED};

pub(super) struct BuildContext<'cfg, 'tools, 'tool> {
    pub(super) cfg: &'cfg RulesConfig,
    pub(super) known_tools: &'tools [&'tool str],
    pub(super) disabled: BTreeSet<&'cfg str>,
    pub(super) winners: Vec<RuleCandidate>,
    pub(super) by_name: HashMap<Name, usize>,
    pub(super) problems: Vec<Problem>,
}
#[derive(Clone, Copy)]
pub(super) enum DirectoryOrigin {
    User,
    Project,
}

impl DirectoryOrigin {
    pub(super) fn for_directory(self, path: &Path) -> Origin {
        match self {
            Self::User => Origin::User(path.to_path_buf()),
            Self::Project => Origin::Project(path.to_path_buf()),
        }
    }

    pub(super) fn for_file(self, path: &Path) -> Origin {
        match self {
            Self::User => Origin::User(path.to_path_buf()),
            Self::Project => Origin::Project(path.to_path_buf()),
        }
    }
}

pub(super) fn add_rule_directory(
    directory: &Path,
    origin: DirectoryOrigin,
    builder: &mut BuildContext<'_, '_, '_>,
) {
    let paths = rule_files(directory, origin, &mut builder.problems);
    for path in paths {
        let file_origin = origin.for_file(&path);
        let Some(bytes) = read_rule_file(&path, &file_origin, &mut builder.problems) else {
            continue;
        };
        let name = file_rule_name(&path);
        match rulefile::parse_rulefile(&name, file_origin.clone(), &bytes) {
            Ok((mut rule, mut notes)) => {
                notes.extend(scope::apply_tool_inventory(
                    &mut rule.scope,
                    builder.known_tools,
                    &file_origin,
                ));
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

pub(super) fn add_plugin_rule_files(files: &[&RuleFile], builder: &mut BuildContext<'_, '_, '_>) {
    let mut counts = HashMap::new();
    let mut capped = HashSet::new();
    for file in files {
        let Some(directory) = plugin_rule_directory(&file.path) else {
            continue;
        };
        let count = counts.entry(directory).or_insert(0_usize);
        if *count == DIRECTORY_LIMIT {
            if capped.insert(directory) {
                builder
                    .problems
                    .push(plugin_directory_cap_problem(file, directory));
            }
            continue;
        }
        *count += 1;
        add_plugin_rule_file(file, builder);
    }
}

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "rule files key on the exact lowercase .md suffix"
)]
fn plugin_rule_directory(path: &str) -> Option<&str> {
    let (directory, file) = path.rsplit_once('/')?;
    (directory.rsplit('/').next() == Some("rules") && file.ends_with(".md")).then_some(directory)
}

fn plugin_directory_cap_problem(file: &RuleFile, directory: &str) -> Problem {
    Problem {
        origin: Origin::Plugin {
            plugin: file.plugin.as_str().into(),
            path: Some(PathBuf::from(directory)),
        },
        kind: ProblemKind::Directory,
        reason: "the directory holds more than 256 rule files".to_owned(),
        consequence: "dalgon read the first 256 by name.".to_owned(),
        severity: Severity::Skipped,
    }
}

pub(super) fn add_plugin_rule_file(file: &RuleFile, builder: &mut BuildContext<'_, '_, '_>) {
    let path = Path::new(file.path.as_ref());
    let origin = Origin::Plugin {
        plugin: file.plugin.as_str().into(),
        path: Some(path.to_path_buf()),
    };
    if file.bytes.len() > FILE_LIMIT {
        builder
            .problems
            .push(file_problem(&origin, "the file is larger than 65536 bytes"));
        return;
    }

    let name = file_rule_name(path);
    match rulefile::parse_rulefile(&name, origin.clone(), file.bytes.as_ref()) {
        Ok((mut rule, mut notes)) => {
            notes.extend(scope::apply_tool_inventory(
                &mut rule.scope,
                builder.known_tools,
                &origin,
            ));
            let (shorthand_fallback, rulebook_globs) = file_metadata(file.bytes.as_ref());
            builder.accept_rule(
                RuleCandidate {
                    rule,
                    rulebook_globs,
                    shorthand_fallback,
                    source_bytes: Some(Arc::clone(&file.bytes)),
                },
                notes,
            );
        }
        Err(rejections) => builder.problems.extend(rejections),
    }
}
pub(super) fn add_record(source: &RecordSource, builder: &mut BuildContext<'_, '_, '_>) {
    let origin = Origin::Record {
        plugin: source.plugin.as_str().into(),
    };
    match validate_record(source, Some(builder.known_tools)) {
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
    pub(super) fn accept_rule(&mut self, candidate: RuleCandidate, notes: Vec<Problem>) {
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

        let new_wins = super::view::origin_preference(
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
fn rule_files(
    directory: &Path,
    origin: DirectoryOrigin,
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

fn directory_problem(directory: &Path, origin: DirectoryOrigin, error: &std::io::Error) -> Problem {
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
