//! Session planner and atomic writer for staged patches.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use dal_core::{CallId, GenerationId, SessionId, TurnId};

use super::{
    ir::{
        Diff, Edit, EngineError, ErrorClass, FindingSeverity, Output, Plan, StagedBatch,
        StagedFile, StagedFileOwned,
    },
    resolve::{check_overlap, resolve_path},
    snapshot::SnapshotStore,
    styles,
};

/// Session context for one patch call.
#[derive(Clone)]
pub struct PatchSession {
    /// Absolute workspace root.
    pub workspace: PathBuf,
    /// Owning session.
    pub session: SessionId,
    /// Session-open generation for reference binding.
    pub generation: GenerationId,
    /// Owning turn.
    pub turn: TurnId,
    /// Provider call identifier.
    pub call: CallId,
    /// Consumer identity for observation ledgers.
    pub consumer: dal_core::Consumer,
    /// Whether symbol/block operations are enabled.
    pub symbols: bool,
    /// Shared line-coverage store.
    pub seen: Arc<crate::Seen>,
    /// Shared search index; dirtied before the first target mutation.
    pub(crate) index: Arc<crate::search::index::Index>,
    /// Shared snapshot reference store.
    pub snapshots: Arc<SnapshotStore>,
    /// Frozen request cutoff for deliveries; later rows never authorize this call.
    /// `None` means the host could not supply a cutoff: Enhanced is rejected
    /// with `observation_unavailable`, Light proceeds on snapshot evidence.
    pub cutoff: Option<u64>,
}

/// Parses, resolves, proves, and stages one payload without writing.
///
/// The returned plan owns complete before/after bytes; observers run on it
/// before any authorization decision.
///
/// # Errors
/// Returns [`EngineError`] when decoding, proving, or staging the payload fails.
pub async fn plan(
    session: &PatchSession,
    style: super::ir::DialectId,
    input: &str,
) -> Result<Plan, EngineError> {
    if input.len() > 8 << 20 {
        return Err(EngineError::new(
            ErrorClass::Limit,
            "patch: input is over 8 MiB. Split the change into several calls.".to_owned(),
        ));
    }
    let edits = styles::parse(style, input, session.symbols)
        .map_err(|error| EngineError::new(ErrorClass::Parse, styles::with_suffix(style, error)))?;
    if edits.is_empty() || edits.len() > 64 {
        return Err(EngineError::new(
            ErrorClass::Limit,
            "patch: a payload may hold at most 64 edits; this one holds more. Split it into several calls.".to_owned(),
        ));
    }
    // Group by canonical path in ascending byte order.
    let mut groups: HashMap<PathBuf, Vec<Edit>> = HashMap::new();
    let mut canonical_of: HashMap<PathBuf, PathBuf> = HashMap::new();
    for edit in edits {
        let raw = edit_path(&edit);
        let (display, canonical) = resolve_path(&session.workspace, raw)?;
        canonical_of.entry(display.clone()).or_insert(canonical);
        groups.entry(display).or_default().push(edit);
    }
    if groups.len() > 64 {
        return Err(EngineError::new(
            ErrorClass::Limit,
            format!(
                "patch: this call touches {} files; the limit is 64. Split it into several calls.",
                groups.len()
            ),
        ));
    }
    let mut displays: Vec<PathBuf> = groups.keys().cloned().collect();
    displays.sort_by(|left, right| {
        left.as_os_str()
            .as_encoded_bytes()
            .cmp(right.as_os_str().as_encoded_bytes())
    });
    let mut files = Vec::with_capacity(displays.len());
    let mut staged_bytes = 0_usize;
    for display in displays {
        let edits = groups.remove(&display).unwrap_or_default();
        let canonical = canonical_of
            .remove(&display)
            .unwrap_or_else(|| display.clone());
        check_overlap(&edits, &display)?;
        let staged = stage::stage_file(session, style, &display, &canonical, edits).await?;
        staged_bytes = staged_bytes.saturating_add(
            staged
                .before
                .as_ref()
                .map_or(0, |bytes| bytes.len())
                .saturating_add(staged.after.as_ref().map_or(0, |bytes| bytes.len())),
        );
        files.push(staged);
    }
    let staged_mib = staged_bytes.div_ceil(1 << 20);
    if staged_bytes > 64 << 20 {
        return Err(EngineError::new(
            ErrorClass::Limit,
            format!(
                "patch: this call stages {staged_mib} MiB; the limit is 64 MiB. Split it into several calls."
            ),
        ));
    }
    // No-op elimination: every file identical is an error.
    if files.iter().all(|file| file.before == file.after) {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            "patch: the edits produce no change.".to_owned(),
        ));
    }
    Ok(Plan {
        style,
        files,
        findings: Vec::new(),
    })
}

pub(crate) async fn plan_replacement(
    session: &PatchSession,
    path: &str,
    before: &[u8],
    after: &[u8],
    line: u32,
) -> Result<Plan, EngineError> {
    let (display, canonical) = resolve_path(&session.workspace, Path::new(path))?;
    let staged = stage::stage_replacement(&display, &canonical, before, after, line).await?;
    if staged.before == staged.after {
        return Err(EngineError::new(
            ErrorClass::Resolve,
            format!("patch: {path}:{line}: replacement produces no change."),
        ));
    }
    Ok(Plan {
        style: super::ir::DialectId::Replace,
        files: vec![staged],
        findings: Vec::new(),
    })
}

/// Runs staged observers and commits the plan atomically.
pub async fn commit(
    session: &PatchSession,
    mut plan: Plan,
    observers: &[Arc<dyn super::ir::EditObserver>],
) -> Output {
    let findings = inspect(session, &plan, observers).await;
    if let Some(blocked) = findings
        .iter()
        .find(|finding| finding.severity == FindingSeverity::Block)
    {
        return Output {
            text: blocked.text.to_string(),
            error_class: Some(ErrorClass::Blocked),
            changes: Vec::new(),
            display: Diff {
                kind: "diff".into(),
                files: Vec::new(),
            },
        };
    }
    plan.findings = findings;
    match commit::apply_files(session, &plan).await {
        Ok(output) => output,
        Err(error) => Output {
            text: error.message.clone(),
            error_class: Some(error.class),
            changes: Vec::new(),
            display: Diff {
                kind: "diff".into(),
                files: Vec::new(),
            },
        },
    }
}

pub(crate) async fn inspect(
    session: &PatchSession,
    plan: &Plan,
    observers: &[Arc<dyn super::ir::EditObserver>],
) -> Vec<super::ir::EditFinding> {
    #[cfg(feature = "symbols")]
    let (pre_parses, post_parses) = cached_parses(session, &plan.files).await;
    #[cfg(feature = "symbols")]
    let views: Vec<StagedFile<'_>> = plan
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| StagedFile {
            path: &file.path,
            absolute_path: &file.absolute_path,
            before: file.before.as_deref(),
            after: file.after.as_deref(),
            hunks: &file.hunks,
            pre_parse: pre_parses[index].clone(),
            post_parse: post_parses[index].clone(),
        })
        .collect();
    #[cfg(not(feature = "symbols"))]
    let views: Vec<StagedFile<'_>> = plan
        .files
        .iter()
        .map(|file| StagedFile {
            path: &file.path,
            absolute_path: &file.absolute_path,
            before: file.before.as_deref(),
            after: file.after.as_deref(),
            hunks: &file.hunks,
        })
        .collect();
    let batch = StagedBatch {
        session: session.session,
        turn: session.turn,
        call: session.call.clone(),
        files: views,
    };
    let mut findings = Vec::new();
    for observer in observers {
        findings.extend(observer.inspect(&batch));
    }
    findings
}

/// Builds pre/post parse snapshots for every staged file from the single
/// shared parser. Used by both pre-approval observer views (`patch.rs`)
/// and commit observer views so both see identical evidence.
#[cfg(feature = "symbols")]
pub(crate) async fn cached_parses(
    session: &PatchSession,
    files: &[StagedFileOwned],
) -> (
    Vec<Option<std::sync::Arc<crate::parse::Parsed>>>,
    Vec<Option<std::sync::Arc<crate::parse::Parsed>>>,
) {
    let mut pre = Vec::with_capacity(files.len());
    let mut post = Vec::with_capacity(files.len());
    for file in files {
        pre.push(cached_parse(&file.path, file.before.as_deref(), session.symbols).await);
        post.push(cached_parse(&file.path, file.after.as_deref(), session.symbols).await);
    }
    (pre, post)
}

#[cfg(feature = "symbols")]
async fn cached_parse(
    path: &std::path::Path,
    bytes: Option<&[u8]>,
    symbols: bool,
) -> Option<std::sync::Arc<crate::parse::Parsed>> {
    if !symbols {
        return None;
    }
    let bytes = bytes?;
    crate::parse::language(path)?;
    match crate::parse::tree(path, bytes).await {
        Ok(parsed) => Some(std::sync::Arc::new(parsed)),
        Err(_) => None,
    }
}

fn edit_path(edit: &Edit) -> &Path {
    match edit {
        Edit::Change { path, .. } | Edit::Create { path, .. } | Edit::Delete { path, .. } => path,
        Edit::Rename { from, .. } => from,
    }
}

pub mod commit;

pub mod stage;
