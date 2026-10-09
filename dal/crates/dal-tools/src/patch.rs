//! Patch tool and edit styles: parsers, engine, and writer.

pub mod ast;
pub mod ir;
pub mod resolve;
pub mod snapshot;
pub mod style;
pub mod styles;
pub mod write;

#[cfg(test)]
mod tests;

pub use ir::{
    DialectId, DiffHunk, DiffLine, DiffLineKind, EditFinding, EditObserver, ErrorClass,
    FindingSeverity, StagedBatch, StagedFile, Tier,
};
pub use style::{EditStyleConfig, EditStyleInput, parse_edit_style, pick};
pub use write::{PatchSession, commit, plan};

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use dal_agent::ToolError;
use dal_agent::ext::tool::ArgError;
use dal_agent::ext::{BoxFuture, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_core::{
    CallId, ModelInfo, Name, Preview, RawJson, RegistrationError, ToolClass, ToolSpec, TurnId,
    Workspace,
};
use sonic_rs::JsonValueTrait as _;

/// Patch tool handle with parsed style table and shared services.
pub struct PatchTool {
    name: Name,
    config: EditStyleConfig,
    specs: std::collections::HashMap<(ir::DialectId, bool), Arc<ToolSpec>>,
    seen: Arc<crate::Seen>,
    index: Arc<crate::search::index::Index>,
    snapshots: Arc<snapshot::SnapshotStore>,
    observer: Option<Arc<dyn EditObserver>>,
    symbols: Arc<AtomicBool>,
}

/// Failure applying a replacement through the patch engine.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ApplyError {
    /// Planning or atomic commit failed in the patch engine.
    #[error(transparent)]
    Engine(#[from] ir::EngineError),
    /// The live tool authorization ladder denied this replacement.
    #[error("patch denied: {0:?}")]
    Blocked(dal_core::DenyReason),
    /// A registered edit observer blocked the staged replacement.
    #[error("patch blocked by observer: {0:?}")]
    ObserverBlocked(ir::EditFinding),
}

/// Applies one exact byte replacement through the patch authorization and commit path.
///
/// Runs `observers` against the immutable staged edit before authorization and
/// again during commit, matching the registered patch tool's observer order.
///
/// # Errors
///
/// Returns an [`ApplyError`] when planning, authorization, observers, or atomic
/// commit rejects the operation.
pub async fn apply_replacement(
    cx: &mut ToolCx<'_>,
    path: &str,
    before: &[u8],
    after: &[u8],
    line: u32,
    observers: &[Arc<dyn ir::EditObserver>],
) -> Result<ir::Output, ApplyError> {
    static NEXT_CALL: AtomicU64 = AtomicU64::new(1);
    let call = CallId::new(format!(
        "apply-replacement-{}",
        NEXT_CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let session = PatchSession {
        workspace: cx.workspace().as_path().to_path_buf(),
        session: cx.session(),
        generation: cx.generation(),
        turn: cx
            .turn()
            .unwrap_or_else(|| TurnId::new(std::num::NonZeroU64::MIN)),
        call,
        consumer: cx.consumer(),
        symbols: false,
        seen: crate::Seen::new(),
        index: crate::search::index::Index::new(None),
        snapshots: Arc::new(snapshot::SnapshotStore::new(process_boot())),
        cutoff: cx.cutoff(),
    };
    let mut plan = write::plan_replacement(&session, path, before, after, line).await?;
    plan.findings = write::inspect(&session, &plan, observers).await;
    if let Some(blocked) = plan
        .findings
        .iter()
        .find(|finding| finding.severity == ir::FindingSeverity::Block)
    {
        return Err(ApplyError::ObserverBlocked(blocked.clone()));
    }
    let preview = preview_for_plan(&plan);
    let _approved = cx.authorize(preview).await.map_err(ApplyError::Blocked)?;
    let output = write::commit(&session, plan, observers).await;
    match output.error_class {
        Some(class) => Err(ApplyError::Engine(ir::EngineError::new(class, output.text))),
        None => Ok(output),
    }
}

impl Tool for PatchTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, model: &ModelInfo) -> Arc<ToolSpec> {
        // Per-model dialect selection; byte-stable per (tool, model).
        // Symbols flag is read from the shared product config once node13
        // supplies it; current specs are symbols-off with the exact plan bytes.
        let style = pick(&self.config, model.route.id());
        // Freeform dialects use the JSON `input` envelope when the provider
        // cannot send custom grammar; replace always uses its JSON schema.
        // Cached exact bytes per (dialect, symbols=false).
        self.specs.get(&(style, false)).cloned().unwrap_or_else(|| {
            self.specs
                .get(&(ir::DialectId::Replace, false))
                .cloned()
                .unwrap_or_else(|| {
                    // Registration guarantees the replace spec; this fallback
                    // is unreachable and never panics on caller input.
                    Arc::new(dal_core::ToolSpec {
                        name: self.name.clone(),
                        description: styles::description(ir::DialectId::Replace, false).into(),
                        parameters: dal_core::RawJson::parse("{}")
                            .unwrap_or(dal_core::RawJson::null()),
                        grammar: None,
                    })
                })
        })
    }

    fn classify(
        &self,
        args: &dal_agent::ext::RawValue,
        _ws: &Workspace,
    ) -> Result<ToolClass, ArgError> {
        // Strict shape check without executing: replace JSON must decode.
        let text = args.as_str();
        if text.trim_start().starts_with('{') {
            let _: styles::replace::ReplaceProbe = sonic_rs::from_str(text)
                .map_err(|error| ArgError::message(format!("patch: invalid input: {error}.")))?;
        }
        Ok(ToolClass::Patch)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move { self.execute(call, cx).await })
    }
}

/// Builds the single `patch` tool over the tools-wide snapshot store.
///
/// The `index` handle is accepted for the planned `Index::dirty` wiring and
/// retained for the writer owner; current staging reads workspace bytes
/// directly. Returns a registration error instead of panicking on fixed
/// schema literals.
pub(crate) fn tool(
    edit_style: &EditStyleInput,
    index: Arc<crate::search::index::Index>,
    seen: Arc<crate::Seen>,
    snapshots: Arc<snapshot::SnapshotStore>,
    observer: Option<Arc<dyn EditObserver>>,
    search_symbols: Arc<AtomicBool>,
) -> Result<Arc<dyn dal_agent::ext::Tool>, dal_core::RegistrationError> {
    let name = dal_core::Name::parse("patch")?;
    let config = parse_edit_style(edit_style).map_err(|_| RegistrationError::InvalidParameters)?;
    // Fixed replace schema bytes from the plan (symbols-off).
    let schema_text = r#"{"type":"object","properties":{"changes":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"object","properties":{"path":{"type":"string","description":"File to change."},"old":{"type":"string","description":"Exact text to replace, copied from read or search output without the <n>: prefixes."},"line":{"type":"integer","minimum":1,"description":"Line where old starts. Needed only when old occurs more than once."},"all":{"type":"boolean","description":"true replaces every occurrence of old. Needs tag."},"new":{"type":"string","description":"Replacement text. Empty removes old."},"tag":{"type":"string","description":"Tag printed with the whole text you replace."},"create":{"type":"string","description":"Content of a new file at path."},"delete":{"type":"boolean","description":"true removes the file at path."},"rename":{"type":"string","description":"New path for the file."}},"required":["path"],"additionalProperties":false}}},"required":["changes"],"additionalProperties":false}"#;
    let parameters =
        RawJson::parse(schema_text).map_err(|_| RegistrationError::InvalidParameters)?;
    if !dal_core::valid_tool_parameters(&parameters) {
        return Err(RegistrationError::InvalidParameters);
    }
    let mut specs = std::collections::HashMap::new();
    for style in [
        ir::DialectId::Anchor,
        ir::DialectId::Replace,
        ir::DialectId::Hashline,
        ir::DialectId::HashlineLight,
        ir::DialectId::HashlineEnhanced,
        ir::DialectId::ApplyPatch,
    ] {
        let (description, schema) = if style == ir::DialectId::Replace {
            (styles::description(style, false), schema_text)
        } else {
            // Freeform JSON envelope for anchor/hashline/apply_patch when the
            // provider cannot send custom grammar.
            (
                styles::description(style, false),
                r#"{"type":"object","properties":{"input":{"type":"string","description":"The whole patch payload."}},"required":["input"],"additionalProperties":false}"#,
            )
        };
        let parameters =
            RawJson::parse(schema).map_err(|_| RegistrationError::InvalidParameters)?;
        if !dal_core::valid_tool_parameters(&parameters) {
            return Err(RegistrationError::InvalidParameters);
        }
        specs.insert(
            (style, false),
            Arc::new(ToolSpec {
                name: name.clone(),
                description: description.into(),
                parameters,
                grammar: None,
            }),
        );
    }
    Ok(Arc::new(PatchTool {
        name,
        config,
        specs,
        seen,
        index,
        snapshots,
        observer,
        symbols: search_symbols,
    }))
}

impl PatchTool {
    async fn execute(&self, call: ToolCall, mut cx: ToolCx<'_>) -> ToolOutcome {
        use dal_core::ToolClass;
        let _ = ToolClass::Patch;
        // Decode dialect for this call: replace JSON vs freeform input.
        // Freeform dialects arrive as {"input":"..."} when the provider
        // cannot send custom grammar; replace is always JSON.
        let args_text = call.args.as_str();
        let (style, payload) = self.select_style(args_text);
        // Build session from host context.
        let workspace = cx.workspace().clone().into_path_buf();
        let session = write::PatchSession {
            workspace,
            session: cx.session(),
            generation: cx.generation(),
            turn: cx
                .turn()
                .unwrap_or(dal_core::TurnId::new(std::num::NonZeroU64::MIN)),
            call: call.id.clone(),
            consumer: cx.consumer(),
            symbols: self.symbols.load(Ordering::Relaxed),
            seen: Arc::clone(&self.seen),
            index: Arc::clone(&self.index),
            snapshots: Arc::clone(&self.snapshots),
            cutoff: cx.cutoff(),
        };
        let mut plan = match write::plan(&session, style, &payload).await {
            Ok(plan) => plan,
            Err(error) => {
                return ToolOutcome::Ok(ToolOutput::from_text(error.message.as_str()));
            }
        };
        let observers: Vec<Arc<dyn ir::EditObserver>> = self.observer.clone().into_iter().collect();
        plan.findings = write::inspect(&session, &plan, &observers).await;
        if let Some(blocked) = plan
            .findings
            .iter()
            .find(|finding| finding.severity == ir::FindingSeverity::Block)
        {
            return ToolOutcome::Ok(ToolOutput::from_text(blocked.text.as_ref()));
        }
        // Staged observers run inside commit; approval binds the exact plan.
        // Preview carries paths, operations, before/after digests, hunks,
        // and findings so approval shows what the user will change.
        let preview = preview_for_plan(&plan);
        // Denials fail closed as the model-visible denial text; tests drive
        // plan/commit directly, not this run() path.
        match cx.authorize(preview).await {
            Ok(_approved) => {
                let output = write::commit(&session, plan, &observers).await;
                let text = output.text.clone();
                // Dirty the search index before returning (best-effort; index
                // owner supplies the exact dirty handle once its public API lands).
                ToolOutcome::Ok(ToolOutput::from_text(text.as_str()))
            }
            Err(deny) => ToolOutcome::Err(ToolError::Denied(deny)),
        }
    }

    fn select_style(&self, args_text: &str) -> (ir::DialectId, String) {
        // If args decode as {"input": "..."} use the configured default style;
        // otherwise treat as replace JSON.
        if let Ok(value) = sonic_rs::from_str::<sonic_rs::Value>(args_text)
            && let Some(input) = value.get("input").and_then(|v| v.as_str())
        {
            let style = match &self.config {
                EditStyleConfig::Scalar(style) => *style,
                EditStyleConfig::Table { default, .. } => *default,
            };
            return (style, input.to_owned());
        }
        // Default to replace for JSON; per-model pick rides the request path
        // once provider cutoffs land. Config scalar fast path:
        let style = match &self.config {
            EditStyleConfig::Scalar(style) => *style,
            EditStyleConfig::Table { default, .. } => *default,
        };
        (style, args_text.to_owned())
    }
}

fn preview_for_plan(plan: &ir::Plan) -> Preview {
    let mut preview_lines = vec![format!("patch {} file(s)", plan.files.len())];
    for file in &plan.files {
        let before_hex = file
            .before
            .as_deref()
            .map_or_else(|| "-".to_owned(), |bytes| blake3::hash(bytes).to_string());
        let after_hex = file
            .after
            .as_deref()
            .map_or_else(|| "-".to_owned(), |bytes| blake3::hash(bytes).to_string());
        preview_lines.push(format!(
            "{:?} {} before={} after={}",
            file.op,
            file.path.display(),
            before_hex,
            after_hex
        ));
        if let Some(dest) = file.renamed_to.as_ref() {
            preview_lines.push(format!("  -> {}", dest.display()));
        }
        for hunk in &file.hunks {
            preview_lines.push(format!(
                "  @@ {}-{} -> {}-{} @@",
                hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
            ));
        }
    }
    for finding in &plan.findings {
        preview_lines.push(format!("[{:?}] {}", finding.severity, finding.text));
    }
    let preview_text = preview_lines.join("\n");
    Preview {
        title: "patch".into(),
        body: preview_text.clone().into(),
        digest: Some(*blake3::hash(preview_text.as_bytes()).as_bytes()),
    }
}

/// A process-scoped boot identity for snapshot references; never all zero,
/// and a restart mints a fresh one.
pub(crate) fn process_boot() -> [u8; 16] {
    let pid = u64::from(std::process::id());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| u64::from(duration.subsec_nanos()));
    let mut boot = [0_u8; 16];
    boot[..8].copy_from_slice(&pid.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());
    boot[8..].copy_from_slice(&nanos.wrapping_mul(0xBF58_476D_1CE4_E5B9).to_le_bytes());
    if boot == [0; 16] {
        boot[0] = 1;
    }
    boot
}
