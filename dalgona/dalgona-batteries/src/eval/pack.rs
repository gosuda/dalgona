// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! The eval-pack loader and check runner.
//!
//! A pack is one `<name>.eval.star` file evaluated as pure Starlark under dal's
//! shared dialect. The pack sees the three `evalpack` functions and the
//! standard language builtins; it has no `dal.*` namespace, no `ctx`, and no
//! service, so a hostile pack can only compute values and fail.

use std::cell::RefCell;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use crate::text::normalize_ws;
use dal_ext::judge::JudgeQuestion;
use dal_star::{CELL_LIMITS, LOAD_LIMITS, Limits, dialect};
use starlark::ErrorKind;
use starlark::environment::{Globals, GlobalsBuilder, LibraryExtension, Module};
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::syntax::AstModule;
use starlark::values::dict::DictRef;
use starlark::values::list::ListRef;
use starlark::values::none::NoneType;
use starlark::values::structs::AllocStruct;
use starlark::values::tuple::TupleRef;
use starlark::values::{FrozenValue, OwnedFrozenValue, Value};
use tokio_util::sync::CancellationToken;

/// The longest prompt a case may carry, in characters.
pub const MAX_PROMPT_CHARS: usize = 8000;
/// The most files a case may seed into its workspace.
pub const MAX_FILES: usize = 64;
/// The largest seeded file, in bytes.
pub const MAX_FILE_BYTES: usize = 65_536;
/// The most repeats one case may request.
pub const MAX_REPEATS: u32 = 10;
/// The repeat count of a case that names none.
pub const DEFAULT_REPEATS: u32 = 3;
/// The largest file `workspace.read` returns, in bytes.
pub const MAX_READ_BYTES: u64 = 1 << 20;

/// The tag that marks a `evalpack.judge(...)` value.
const JUDGE_TAG: &str = "evalpack.judge";

/// One eval pack: its declared name and its cases in declaration order.
pub struct Pack {
    /// The name the pack passed to `evalpack.pack`.
    pub name: String,
    /// The cases, in declaration order.
    pub cases: Vec<Case>,
}

/// One validated case.
pub struct Case {
    /// The case id, unique in its pack.
    pub id: String,
    /// The prompt the session runs.
    pub prompt: String,
    /// Workspace files written before the prompt runs, as relative path and text.
    pub files: Vec<(String, String)>,
    /// How many times each arm runs the case, from 1 to 10.
    pub repeats: u32,
    /// The tool names the run may use; `None` means the product default set.
    pub tools: Option<Vec<String>>,
    /// How a finished run is scored.
    pub scoring: Scoring,
}

/// How a finished run is scored.
pub enum Scoring {
    /// A Starlark function of `(result, workspace)` that returns a bool.
    Check(Check),
    /// A one-line bool question for the judge.
    Judge(String),
}

/// A frozen Starlark `check` function, kept alive with its module heap.
pub struct Check(OwnedFrozenValue);

impl std::fmt::Debug for Check {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Check(..)")
    }
}

/// How a session ended, as a check sees it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Exit {
    /// The turn ended normally.
    Ok,
    /// The turn or the session failed.
    Failed,
    /// The turn was cancelled.
    Cancelled,
}

impl Exit {
    /// The text a check compares `result["exit"]` against.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One tool call a session made.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolCall {
    /// The tool name.
    pub name: String,
    /// The call arguments as JSON text.
    pub args: String,
}

/// Token counts and the provider-reported cost of one run.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// The provider-reported cost in USD, when reported.
    pub cost_usd: Option<f64>,
}

/// What a check inspects: the check's `result` argument.
#[derive(Clone, Copy, Debug)]
pub struct CheckInput<'a> {
    /// How the session ended.
    pub exit: Exit,
    /// The assistant text of the run.
    pub text: &'a str,
    /// Every tool call, in order.
    pub tool_calls: &'a [ToolCall],
    /// The run usage.
    pub usage: Usage,
}

/// A Starlark failure with the 1-based source line it names.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("starlark error at line {line}: {message}")]
pub struct ScriptError {
    /// The 1-based line of the failing statement.
    pub line: usize,
    /// The failure text, on one line.
    pub message: String,
}

/// Why a check produced no verdict.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CheckError {
    /// The check raised; carries the Starlark failure.
    #[error(transparent)]
    Script(#[from] ScriptError),
    /// An interrupt fired while the check ran.
    #[error("interrupted")]
    Interrupted,
    /// The run deadline passed while the check ran.
    #[error("the check exceeded the run time budget")]
    Timeout,
}

/// A pack that cannot be loaded. The `Display` text carries no pack prefix.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The file did not call `evalpack.pack` exactly once.
    #[error("expected exactly one evalpack.pack call, found {0}.")]
    PackCalls(usize),
    /// A case failed its validation.
    #[error("case \"{id}\": {reason}.")]
    Case {
        /// The case id, empty when the id itself was invalid.
        id: String,
        /// Why the case was refused.
        reason: String,
    },
    /// The source did not parse, raised, or ran out of budget.
    #[error("{0}.")]
    Script(ScriptError),
}

/// An error raised by an `evalpack` builtin.
#[derive(Debug, thiserror::Error)]
enum NativeError {
    #[error("case \"{id}\": {reason}")]
    Case { id: String, reason: String },
    #[error("evalpack.pack: name must be a non-empty string")]
    PackName,
    #[error("evalpack.pack: cases must be a list of evalpack.case(...) values")]
    PackCases,
    #[error("evalpack.case: id must be a non-empty string")]
    CaseId,
    #[error("workspace.read: {0}")]
    Read(String),
    #[error("check returned {0}, not a boolean")]
    CheckVerdict(String),
}

/// One validated case before its check is frozen.
struct CaseSpec {
    id: String,
    prompt: String,
    files: Vec<(String, String)>,
    repeats: u32,
    tools: Option<Vec<String>>,
    judge: Option<String>,
}

struct PackSpec {
    name: String,
    cases: Vec<usize>,
}

#[derive(Default)]
struct Registry {
    cases: Vec<CaseSpec>,
    packs: Vec<PackSpec>,
}

thread_local! {
    /// The cases and pack calls of the load running on this thread.
    static REGISTRY: RefCell<Registry> = const { RefCell::new(Registry { cases: Vec::new(), packs: Vec::new() }) };
    /// The workspace the check running on this thread reads from.
    static WORKSPACE: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// Clears a thread-local slot when the evaluation that filled it ends.
struct Reset<F: FnMut()>(F);

impl<F: FnMut()> Drop for Reset<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

#[starlark_module]
fn evalpack_builtins(builder: &mut GlobalsBuilder) {
    /// Declares the pack: its name and its cases.
    fn pack<'v>(name: &str, cases: Value<'v>) -> starlark::Result<NoneType> {
        declare_pack(name, cases)?;
        Ok(NoneType)
    }

    /// Declares one case; returns an opaque handle for `evalpack.pack`.
    fn case<'v>(
        id: &str,
        prompt: &str,
        check: Value<'v>,
        files: Option<Value<'v>>,
        repeats: Option<i32>,
        tools: Option<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<i32> {
        declare_case(id, prompt, check, files, repeats, tools, eval)
    }

    /// Builds a judge-scored `check` from one bool question.
    fn judge<'v>(question: &str, eval: &mut Evaluator<'v, '_, '_>) -> starlark::Result<Value<'v>> {
        if let Err(error) = JudgeQuestion::bool(question) {
            return Err(native(NativeError::Case {
                id: String::new(),
                reason: error.to_string(),
            }));
        }
        Ok(eval.heap().alloc((JUDGE_TAG, question)))
    }
}

#[starlark_module]
fn workspace_builtins(builder: &mut GlobalsBuilder) {
    /// Reads one text file of the case workspace.
    fn workspace_read(path: &str) -> starlark::Result<String> {
        read_workspace_file(path).map_err(native)
    }
}

fn native(error: NativeError) -> starlark::Error {
    starlark::Error::new_native(error)
}

fn case_error(id: &str, reason: impl Into<String>) -> starlark::Error {
    native(NativeError::Case {
        id: id.to_owned(),
        reason: reason.into(),
    })
}

fn declare_pack(name: &str, cases: Value<'_>) -> starlark::Result<()> {
    if name.is_empty() {
        return Err(native(NativeError::PackName));
    }
    let list = ListRef::from_value(cases).ok_or_else(|| native(NativeError::PackCases))?;
    REGISTRY.with_borrow_mut(|registry| {
        let mut indices = Vec::with_capacity(list.len());
        for item in list.iter() {
            let index = item
                .unpack_i32()
                .and_then(|raw| usize::try_from(raw).ok())
                .filter(|index| *index < registry.cases.len())
                .ok_or_else(|| native(NativeError::PackCases))?;
            let id = &registry.cases[index].id;
            if indices
                .iter()
                .any(|seen: &usize| registry.cases[*seen].id == *id)
            {
                return Err(case_error(id, "duplicate id"));
            }
            indices.push(index);
        }
        registry.packs.push(PackSpec {
            name: name.to_owned(),
            cases: indices,
        });
        Ok(())
    })
}

fn declare_case<'v>(
    id: &str,
    prompt: &str,
    check: Value<'v>,
    files: Option<Value<'v>>,
    repeats: Option<i32>,
    tools: Option<Value<'v>>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<i32> {
    if id.is_empty() {
        return Err(native(NativeError::CaseId));
    }
    let prompt_chars = prompt.chars().count();
    if !(1..=MAX_PROMPT_CHARS).contains(&prompt_chars) {
        return Err(case_error(
            id,
            format!("prompt must be 1 to {MAX_PROMPT_CHARS} characters"),
        ));
    }
    let repeats = match repeats {
        None => DEFAULT_REPEATS,
        Some(raw) => u32::try_from(raw)
            .ok()
            .filter(|value| (1..=MAX_REPEATS).contains(value))
            .ok_or_else(|| {
                case_error(
                    id,
                    format!("repeats must be an integer from 1 to {MAX_REPEATS}"),
                )
            })?,
    };
    let files = match files {
        Some(value) if !value.is_none() => read_files(id, value)?,
        _ => Vec::new(),
    };
    let tools = match tools {
        Some(value) if !value.is_none() => Some(read_tools(id, value)?),
        _ => None,
    };
    let judge = read_scoring(id, check)?;
    let is_check = judge.is_none();
    let index = REGISTRY.with_borrow_mut(|registry| {
        registry.cases.push(CaseSpec {
            id: id.to_owned(),
            prompt: prompt.to_owned(),
            files,
            repeats,
            tools,
            judge,
        });
        registry.cases.len() - 1
    });
    if is_check {
        eval.module().set(&check_name(index), check);
    }
    i32::try_from(index).map_err(|_| native(NativeError::PackCases))
}

/// The module variable that keeps case `index`'s check alive through freezing.
fn check_name(index: usize) -> String {
    format!("evalpack.check.{index}")
}

fn read_judge(check: Value<'_>) -> Option<String> {
    let tuple = TupleRef::from_value(check)?;
    match tuple.content() {
        [tag, question] if tag.unpack_str() == Some(JUDGE_TAG) => {
            question.unpack_str().map(str::to_owned)
        }
        _ => None,
    }
}

/// Returns the judge question of a judge-scored check, or `None` for a function.
fn read_scoring(id: &str, check: Value<'_>) -> starlark::Result<Option<String>> {
    if let Some(question) = read_judge(check) {
        return Ok(Some(question));
    }
    if matches!(check.get_type(), "function" | "native_function") {
        return Ok(None);
    }
    Err(case_error(
        id,
        "check must be a function of (result, workspace) or evalpack.judge(question)",
    ))
}

fn read_files(id: &str, value: Value<'_>) -> starlark::Result<Vec<(String, String)>> {
    let dict = DictRef::from_value(value)
        .ok_or_else(|| case_error(id, "files must be a dict of relative path to text content"))?;
    if dict.len() > MAX_FILES {
        return Err(case_error(
            id,
            format!("files has more than {MAX_FILES} entries"),
        ));
    }
    let mut files = Vec::with_capacity(dict.len());
    for (key, content) in dict.iter() {
        let (Some(path), Some(content)) = (key.unpack_str(), content.unpack_str()) else {
            return Err(case_error(
                id,
                "files must be a dict of relative path to text content",
            ));
        };
        if !is_relative(path) {
            return Err(case_error(id, "files paths must be relative"));
        }
        if content.len() > MAX_FILE_BYTES {
            return Err(case_error(
                id,
                format!("file \"{path}\" is larger than {MAX_FILE_BYTES} bytes"),
            ));
        }
        files.push((path.to_owned(), content.to_owned()));
    }
    Ok(files)
}

fn read_tools(id: &str, value: Value<'_>) -> starlark::Result<Vec<String>> {
    let list = ListRef::from_value(value)
        .ok_or_else(|| case_error(id, "tools must be a list of tool names"))?;
    let mut tools = Vec::with_capacity(list.len());
    for item in list.iter() {
        match item.unpack_str() {
            Some(name) if !name.is_empty() => tools.push(name.to_owned()),
            _ => return Err(case_error(id, "tools must be a list of tool names")),
        }
    }
    Ok(tools)
}

/// Whether `path` names a file strictly inside a workspace: no root, no
/// drive, no `..`, and a final normal component.
fn is_relative(path: &str) -> bool {
    let path = Path::new(path);
    let mut components = path.components().peekable();
    if components.peek().is_none() {
        return false;
    }
    components.all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
        && matches!(path.components().next_back(), Some(Component::Normal(_)))
}

fn read_workspace_file(path: &str) -> Result<String, NativeError> {
    if !is_relative(path) {
        return Err(NativeError::Read(format!(
            "{path} must be a relative path inside the workspace"
        )));
    }
    let root = WORKSPACE
        .with_borrow(Clone::clone)
        .ok_or_else(|| NativeError::Read("no workspace is attached".to_owned()))?;
    let missing = |_: std::io::Error| NativeError::Read(format!("no such file: {path}"));
    let target = root.join(path).canonicalize().map_err(missing)?;
    let root = root.canonicalize().map_err(missing)?;
    if !target.starts_with(&root) {
        return Err(NativeError::Read(format!(
            "{path} resolves outside the workspace"
        )));
    }
    let meta = std::fs::metadata(&target).map_err(missing)?;
    if !meta.is_file() {
        return Err(NativeError::Read(format!("no such file: {path}")));
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(NativeError::Read(format!(
            "{path} is larger than {MAX_READ_BYTES} bytes"
        )));
    }
    let bytes = std::fs::read(&target).map_err(missing)?;
    String::from_utf8(bytes)
        .map_err(|_| NativeError::Read(format!("{path} is not valid UTF-8 text")))
}

/// The globals a pack sees: the standard builtins, `json`, and `evalpack`.
fn pack_globals() -> Globals {
    GlobalsBuilder::extended_by(&[LibraryExtension::Json])
        .with_namespace("evalpack", evalpack_builtins)
        .build()
}

fn apply_limits(evaluator: &mut Evaluator<'_, '_, '_>, limits: Limits) -> Result<(), ScriptError> {
    fn refused(error: impl std::fmt::Display) -> ScriptError {
        ScriptError {
            line: 1,
            message: error.to_string(),
        }
    }
    let heap = usize::try_from(limits.heap_bytes).unwrap_or(usize::MAX);
    let depth = usize::try_from(limits.stack_depth).unwrap_or(usize::MAX);
    evaluator
        .set_max_tick_count(limits.ticks)
        .map_err(refused)?;
    evaluator.set_max_heap_size(heap).map_err(refused)?;
    evaluator.set_max_callstack_size(depth).map_err(refused)?;
    Ok(())
}

fn script_error(error: &starlark::Error) -> ScriptError {
    let line = error
        .span()
        .map_or(1, |span| span.resolve_span().begin.line + 1);
    let text = error.without_diagnostic().to_string();
    let message = normalize_ws(&text);
    ScriptError {
        line,
        message: message.trim_end_matches('.').to_owned(),
    }
}

fn classify(error: &starlark::Error) -> LoadError {
    if let ErrorKind::Native(inner) = error.kind()
        && let Some(NativeError::Case { id, reason }) = inner.downcast_ref::<NativeError>()
    {
        return LoadError::Case {
            id: id.clone(),
            reason: reason.clone(),
        };
    }
    LoadError::Script(script_error(error))
}

/// Evaluates one pack source and returns its validated cases.
///
/// `file_name` is only used in Starlark diagnostics.
///
/// # Errors
/// Returns [`LoadError`] when the source does not parse, raises, exceeds its
/// budget, does not call `evalpack.pack` exactly once, or declares an invalid
/// case.
pub fn load(file_name: &str, source: &str) -> Result<Pack, LoadError> {
    let ast = AstModule::parse(file_name, source.to_owned(), &dialect())
        .map_err(|error| classify(&error))?;
    REGISTRY.with_borrow_mut(|registry| *registry = Registry::default());
    let _clear = Reset(|| REGISTRY.with_borrow_mut(|registry| *registry = Registry::default()));
    let frozen = Module::with_temp_heap(|module| {
        {
            let mut evaluator = Evaluator::new(&module);
            apply_limits(&mut evaluator, LOAD_LIMITS).map_err(LoadError::Script)?;
            evaluator
                .eval_module(ast, &pack_globals())
                .map_err(|error| classify(&error))?;
        }
        module.freeze().map_err(|error| {
            LoadError::Script(ScriptError {
                line: 1,
                message: format!("{error:?}"),
            })
        })
    })?;
    let mut registry = REGISTRY.with_borrow_mut(std::mem::take);
    let found = registry.packs.len();
    let (Some(spec), 1) = (registry.packs.pop(), found) else {
        return Err(LoadError::PackCalls(found));
    };
    let mut cases = Vec::with_capacity(spec.cases.len());
    for index in spec.cases {
        let case = &mut registry.cases[index];
        let scoring = if let Some(question) = case.judge.take() {
            Scoring::Judge(question)
        } else {
            let value = frozen.get(&check_name(index)).map_err(|error| {
                LoadError::Script(ScriptError {
                    line: 1,
                    message: error.to_string(),
                })
            })?;
            Scoring::Check(Check(value))
        };
        cases.push(Case {
            id: std::mem::take(&mut case.id),
            prompt: std::mem::take(&mut case.prompt),
            files: std::mem::take(&mut case.files),
            repeats: case.repeats,
            tools: case.tools.take(),
            scoring,
        });
    }
    Ok(Pack {
        name: spec.name,
        cases,
    })
}

#[derive(serde::Serialize)]
struct ResultJson<'a> {
    exit: &'static str,
    text: &'a str,
    tool_calls: Vec<CallJson<'a>>,
    usage: UsageJson,
}

#[derive(serde::Serialize)]
struct CallJson<'a> {
    name: &'a str,
    args: sonic_rs::Value,
}

#[derive(serde::Serialize)]
struct UsageJson {
    input_tokens: u64,
    output_tokens: u64,
    cost_usd: Option<f64>,
}

/// Maps a failed check evaluation: an interrupt wins over the deadline, and
/// either wins over the Starlark failure. Both conditions are monotonic, so a
/// stop the evaluator observed still holds here.
fn classify_stop(
    error: &starlark::Error,
    cancel: &CancellationToken,
    deadline: Instant,
) -> CheckError {
    if cancel.is_cancelled() {
        CheckError::Interrupted
    } else if Instant::now() >= deadline {
        CheckError::Timeout
    } else {
        CheckError::Script(script_error(error))
    }
}

impl Check {
    /// Runs the check against a finished run and its workspace, observing
    /// `cancel` and `deadline` through the evaluator.
    ///
    /// A `true` result passes the run; any other truthiness fails it.
    ///
    /// # Errors
    /// Returns [`CheckError::Script`] when the check raises, reads a file the
    /// run never wrote, or exceeds its budget; [`CheckError::Interrupted`] on
    /// interrupt; [`CheckError::Timeout`] past `deadline`.
    pub fn run(
        &self,
        input: &CheckInput<'_>,
        workspace: &Path,
        cancel: &CancellationToken,
        deadline: Instant,
    ) -> Result<bool, CheckError> {
        let payload = sonic_rs::to_string(&ResultJson {
            exit: input.exit.as_str(),
            text: input.text,
            tool_calls: input
                .tool_calls
                .iter()
                .map(|call| CallJson {
                    name: &call.name,
                    args: sonic_rs::from_str(&call.args).unwrap_or_default(),
                })
                .collect(),
            usage: UsageJson {
                input_tokens: input.usage.input_tokens,
                output_tokens: input.usage.output_tokens,
                cost_usd: input.usage.cost_usd,
            },
        })
        .map_err(|error| ScriptError {
            line: 1,
            message: error.to_string(),
        })?;
        WORKSPACE.with_borrow_mut(|slot| *slot = Some(workspace.to_path_buf()));
        let _detach = Reset(|| WORKSPACE.with_borrow_mut(|slot| *slot = None));
        match self.call(&payload, cancel.clone(), deadline) {
            Ok(verdict) => Ok(verdict),
            Err(error) => Err(classify_stop(&error, cancel, deadline)),
        }
    }

    fn call(
        &self,
        payload: &str,
        stop: CancellationToken,
        deadline: Instant,
    ) -> starlark::Result<bool> {
        let globals = pack_globals();
        let internal = GlobalsBuilder::new().with(workspace_builtins).build();
        let read = internal
            .iter()
            .find(|(name, _)| *name == "workspace_read")
            .map(|(_, value)| value)
            .ok_or_else(|| native(NativeError::Read("workspace is unavailable".to_owned())))?;
        let function = frozen_value(&self.0);
        Module::with_temp_heap(|module| {
            module.frozen_heap().add_reference(internal.heap());
            module.frozen_heap().add_reference(globals.heap());
            module.frozen_heap().add_reference(self.0.owner());
            let heap = module.heap();
            module.set("payload", heap.alloc_str(payload).to_value());
            let decode =
                AstModule::parse("result.star", "json.decode(payload)".to_owned(), &dialect())?;
            let mut evaluator = Evaluator::new(&module);
            evaluator.set_check_cancelled(Box::new(move || {
                stop.is_cancelled() || Instant::now() >= deadline
            }));
            apply_limits(&mut evaluator, CELL_LIMITS).map_err(starlark::Error::new_other)?;
            let result = evaluator.eval_module(decode, &globals)?;
            let workspace = heap.alloc(AllocStruct([("read", read.to_value())]));
            let verdict =
                evaluator.eval_function(function.to_value(), &[result, workspace], &[])?;
            verdict
                .unpack_bool()
                .ok_or_else(|| native(NativeError::CheckVerdict(verdict.get_type().to_owned())))
        })
    }
}

/// The frozen value inside `owned`. Callers keep `owned` alive for as long as
/// they use the result: its owner heap is what keeps the value valid.
fn frozen_value(owned: &OwnedFrozenValue) -> FrozenValue {
    let mut out = None;
    owned.map(|value| {
        out = Some(value);
        value
    });
    out.unwrap_or_else(FrozenValue::new_none)
}

#[cfg(test)]
mod tests;
