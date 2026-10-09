//! Session command execution: typed host operations and slash dispatch.
//!
//! [`run_command`] runs [`Effect::Command`](dal_core::Effect::Command)
//! off the actor task so handler [`submit_wait`](CommandCx::submit_wait)
//! calls never deadlock against the actor loop. `Run` commands dispatch
//! through the generation's handler table; job cancellation, export,
//! scoped models, fork, and clone run directly; reload routes through the
//! registered reload handler.

pub(crate) mod host;

use std::ffi::OsString;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dal_core::{
    Block, CancelScope, ClientId, Command, CommandName, EntryKind, EntryView, ExportFormat, JobId,
    JournalPart, ModelRoute, Origin, Output, Reply, View, Workspace,
};
use host::driver_host;

use crate::error::{AgentError, ValidationError};
use crate::ext::command::CommandCx;
use crate::ext::{Caller, CallerKind};
use crate::session::actor::TurnWork;
use crate::session::driver::DriverDeps;
use crate::session::tasks::SessionTasks;

/// Runs one actor-bound command effect and reports its client reply.
pub(crate) async fn run_command(
    deps: &DriverDeps,
    scoped: &mut Option<Vec<Box<str>>>,
    route: Option<ModelRoute>,
    cmd: Command,
    by: ClientId,
) {
    let result = execute(deps, scoped, route, cmd, by).await;
    let _ = deps.handle.work(TurnWork::CommandDone { result }).await;
}

/// Executes one command effect to its client reply.
async fn execute(
    deps: &DriverDeps,
    scoped: &mut Option<Vec<Box<str>>>,
    route: Option<ModelRoute>,
    cmd: Command,
    by: ClientId,
) -> Result<Reply, AgentError> {
    match cmd {
        Command::Run { name, args, .. } => {
            run_handler(deps, scoped.as_deref(), route, &name, &args).await
        }
        Command::Cancel { scope } => match scope {
            CancelScope::Job(id) => {
                cancel_job(deps, id).await;
                Ok(Reply::Done(Output::Nothing))
            }
            CancelScope::Turn(_) => Err(invalid("this cancel scope runs in the actor.")),
        },
        Command::Export { path, format } => export(deps, path, format).await,
        Command::SetScopedModels(ids) => {
            *scoped = if ids.is_empty() { None } else { Some(ids) };
            Ok(Reply::Done(Output::Nothing))
        }
        Command::Fork(entry) => branch(deps, Some(entry), by).await,
        Command::Clone => branch(deps, None, by).await,
        Command::ReloadPlugins => run_handler(deps, scoped.as_deref(), route, "reload", "").await,
        _ => Err(invalid("this command runs in the actor.")),
    }
}

/// Builds an invalid client error with product text.
fn invalid(message: &str) -> AgentError {
    AgentError::Invalid(ValidationError::new(message))
}

/// Forks or clones the session, reporting the new session identity.
async fn branch(
    deps: &DriverDeps,
    at: Option<dal_core::EntryId>,
    by: ClientId,
) -> Result<Reply, AgentError> {
    let id = deps.handle.branch(at, by).await?;
    Ok(Reply::Done(Output::Text(id.to_string().into())))
}

/// Runs one registered slash handler for a `Run` command.
async fn run_handler(
    deps: &DriverDeps,
    scoped: Option<&[Box<str>]>,
    route: Option<ModelRoute>,
    name: &str,
    args: &str,
) -> Result<Reply, AgentError> {
    let cmd = CommandName::parse(name).map_err(|_| invalid(&format!("unknown command {name}.")))?;
    let generation = deps.host.shared.generation.borrow().clone();
    let (spec, handler) = generation
        .command(&cmd)
        .ok_or_else(|| invalid(&format!("no command registered for {name}.")))?;
    let _ = spec;
    let owner = generation
        .extensions
        .iter()
        .find(|ext| {
            ext.commands()
                .iter()
                .any(|(_, other)| Arc::ptr_eq(other, handler))
        })
        .ok_or_else(|| invalid(&format!("command {name} has no owning extension.")))?;
    let caller = Caller::new(
        owner
            .name()
            .parse()
            .map_err(|_| invalid("command owner name is invalid."))?,
        owner.origin(),
        owner.inject(),
        owner.state_version(),
        CallerKind::Handler,
        None,
    );
    let host = Arc::new(driver_host(deps, scoped, route).await);
    let cx = CommandCx::new(caller, deps.session, None, Arc::clone(&deps.services), host);
    // One script host per command run, against the current generation; its
    // uncaptured environment is the empty authority, so a scripted command
    // handler's own declaration D bounds it (E01 R10).
    let cx = match crate::session::script::SessionScriptHost::for_generation(
        deps.session,
        &deps.backend,
        std::sync::Arc::clone(&deps.host.shared.interpreters),
        deps.host.shared.generation.borrow().clone(),
    )
    .attach(None)
    {
        Some(script) => cx.with_script(script),
        None => cx,
    };
    handler
        .run(args, cx)
        .await
        .map_err(|error| invalid(&error.to_string()))
}

/// Cancels one background job without failing a missing row.
async fn cancel_job(deps: &DriverDeps, id: JobId) {
    let mut table = deps.jobs.lock().await;
    let _ = table.cancel(id);
}

const EXPORT_PATH_DENIED: &str = "export paths outside the workspace require a built-in command";

fn untrusted_export_path_denied(origin: Origin, command: &Command) -> bool {
    origin != Origin::Builtin
        && matches!(
            command,
            Command::Export {
                path: Some(path),
                ..
            } if path.is_absolute() || has_parent_traversal(path)
        )
}
async fn relative_export_path_escapes_workspace(workspace: &Workspace, target: &Path) -> bool {
    let Some(mut parent) = target.parent().map(Path::to_path_buf) else {
        return true;
    };
    let Ok(root) = tokio::fs::canonicalize(workspace.as_path()).await else {
        return true;
    };
    loop {
        match tokio::fs::canonicalize(&parent).await {
            Ok(canonical) => return !canonical.starts_with(&root),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(next) = parent.parent().map(Path::to_path_buf) else {
                    return true;
                };
                if next == parent {
                    return true;
                }
                parent = next;
            }
            Err(_) => return true,
        }
    }
}

/// Exports the current leaf entries to a file.
async fn export(
    deps: &DriverDeps,
    path: Option<PathBuf>,
    format: ExportFormat,
) -> Result<Reply, AgentError> {
    let relative_path = path
        .as_ref()
        .is_some_and(|path| !path.is_absolute() && !has_parent_traversal(path));
    let target = export_target(&deps.workspace, path, format);
    if relative_path && relative_export_path_escapes_workspace(&deps.workspace, &target).await {
        return Err(invalid(EXPORT_PATH_DENIED));
    }
    let host = driver_host(deps, None, None).await;
    let (view, entries) = host.leaf_export_snapshot().await;
    let text = match format {
        ExportFormat::Markdown => render_markdown(&view, &entries, &jiff::Zoned::now())
            .map_err(|error| invalid(&error.to_string()))?,
        ExportFormat::Jsonl => {
            render_jsonl(&entries).map_err(|error| invalid(&format!("export failed: {error}")))?
        }
    };
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| invalid(&format!("export failed: {error}")))?;
    }
    write_export(&deps.tasks, &target, text.as_bytes())
        .await
        .map_err(|error| export_write_error(&target, error))?;
    Ok(Reply::Done(Output::Text(
        export_success_message(&target).into(),
    )))
}

fn export_success_message(target: &Path) -> String {
    format!("Exported the session to {}.", target.display())
}

fn export_target(workspace: &Workspace, path: Option<PathBuf>, format: ExportFormat) -> PathBuf {
    match path {
        Some(path) if path.is_absolute() => path,
        Some(path) => workspace.as_path().join(path),
        None => workspace.as_path().join(match format {
            ExportFormat::Markdown => "dal-export.md",
            ExportFormat::Jsonl => "dal-export.jsonl",
        }),
    }
}

fn export_write_error(target: &Path, error: ExportWriteError) -> AgentError {
    match error {
        ExportWriteError::TargetExists => invalid(&format!(
            "export target already exists: {}",
            target.display()
        )),
        ExportWriteError::PublishedButNotDurable(source) => invalid(&format!(
            "export target was published to {} but directory sync failed: {source}",
            target.display()
        )),
        ExportWriteError::Io(source) => invalid(&format!("export failed: {source}")),
    }
}

fn has_parent_traversal(path: &Path) -> bool {
    path.components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
}

#[derive(Debug, thiserror::Error)]
enum ExportWriteError {
    #[error("target exists")]
    TargetExists,
    #[error("target was published but directory sync failed: {0}")]
    PublishedButNotDurable(#[source] io::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

static NEXT_EXPORT_TEMP: AtomicU64 = AtomicU64::new(0);

fn export_temp_path(target: &Path, sequence: u64) -> io::Result<PathBuf> {
    let Some(name) = target.file_name() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "export target has no file name",
        ));
    };
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".dalgon-{}-{sequence}.tmp", std::process::id()));
    Ok(parent.join(temp_name))
}

async fn open_export_temp(target: &Path) -> io::Result<(PathBuf, tokio::fs::File)> {
    loop {
        let sequence = NEXT_EXPORT_TEMP
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| io::Error::other("export temporary-file sequence exhausted"))?;
        let temp = export_temp_path(target, sequence)?;
        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .await
        {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
}

struct TempFileCleanup {
    path: PathBuf,
    tasks: SessionTasks,
    handle: tokio::runtime::Handle,
    armed: bool,
}

impl TempFileCleanup {
    fn new(path: PathBuf, tasks: SessionTasks, handle: tokio::runtime::Handle) -> Self {
        Self {
            path,
            tasks,
            handle,
            armed: true,
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    async fn remove(&mut self) -> io::Result<()> {
        tokio::fs::remove_file(&self.path).await?;
        self.armed = false;
        Ok(())
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileCleanup {
    fn drop(&mut self) {
        if self.armed {
            let path = std::mem::take(&mut self.path);
            self.tasks.spawn_cleanup(&self.handle, async move {
                let _ = tokio::fs::remove_file(path).await;
            });
        }
    }
}

async fn write_export(
    tasks: &SessionTasks,
    target: &Path,
    bytes: &[u8],
) -> Result<(), ExportWriteError> {
    use tokio::io::AsyncWriteExt as _;

    let (temp, mut file) = open_export_temp(target).await?;
    let mut cleanup = TempFileCleanup::new(temp, tasks.clone(), tokio::runtime::Handle::current());
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if let Err(error) = file.write_all(bytes).await {
        drop(file);
        let _ = cleanup.remove().await;
        return Err(ExportWriteError::Io(error));
    }
    if let Err(error) = file.sync_all().await {
        drop(file);
        let _ = cleanup.remove().await;
        return Err(ExportWriteError::Io(error));
    }
    drop(file);
    if let Err(error) = publish_export_noclobber(cleanup.path(), target).await {
        let _ = cleanup.remove().await;
        return if error.kind() == io::ErrorKind::AlreadyExists {
            Err(ExportWriteError::TargetExists)
        } else {
            Err(ExportWriteError::Io(error))
        };
    }
    cleanup.disarm();
    sync_export_dir(parent)
        .await
        .map_err(ExportWriteError::PublishedButNotDurable)?;
    Ok(())
}

async fn publish_export_noclobber(temp: &Path, target: &Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let temp = temp.to_path_buf();
        let target = target.to_path_buf();
        tokio::task::spawn_blocking(move || {
            rustix::fs::renameat_with(
                rustix::fs::CWD,
                temp,
                rustix::fs::CWD,
                target,
                rustix::fs::RenameFlags::NOREPLACE,
            )
        })
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::from)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        tokio::fs::hard_link(temp, target).await?;
        tokio::fs::remove_file(temp).await
    }
}

#[cfg(windows)]
fn sync_export_dir(_path: &Path) -> std::future::Ready<io::Result<()>> {
    std::future::ready(Ok(()))
}

#[cfg(not(windows))]
async fn sync_export_dir(path: &Path) -> io::Result<()> {
    tokio::fs::File::open(path).await?.sync_all().await
}

fn render_markdown(
    view: &View,
    entries: &[EntryView],
    at: &jiff::Zoned,
) -> Result<String, MarkdownError> {
    let id = view.session.id.to_string();
    let short_id: String = id.chars().take(8).collect();
    let workspace = view.session.workspace.as_path().display();
    let mut output = String::with_capacity(256);
    if let Some(name) = view.session.name.as_deref() {
        output.push_str("# ");
        output.push_str(name);
    } else {
        output.push_str("# Session ");
        output.push_str(&short_id);
    }
    let _ = write!(
        output,
        "\n\nSession {id} · exported {} · {workspace}",
        at.strftime("%Y-%m-%d %H:%M")
    );

    for entry in entries {
        match &entry.kind {
            EntryKind::User { parts } => {
                let body = render_parts(parts)?;
                let mut section = String::from("## You\n\n");
                section.push_str(&body);
                append_section(&mut output, &section);
            }
            EntryKind::Assistant { content, .. } => {
                if let Some(section) = assistant_section(content) {
                    append_section(&mut output, &section);
                }
            }
            EntryKind::ToolResult {
                name, error, parts, ..
            } => {
                let mut section = String::new();
                let state = if *error { "failed" } else { "ok" };
                let _ = write!(section, "### Tool result: {name} · {state}\n\n");
                section.push_str(&fence(&render_parts(parts)?, "text"));
                append_section(&mut output, &section);
            }
            EntryKind::Compaction { summary, .. } => {
                append_summary(&mut output, summary.as_deref());
            }
            EntryKind::Reminder { .. }
            | EntryKind::Model { .. }
            | EntryKind::Thinking { .. }
            | EntryKind::Mode { .. }
            | EntryKind::Approval { .. }
            | EntryKind::BranchSummary { .. } => {}
        }
    }
    output.push('\n');
    Ok(output)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
enum MarkdownError {
    #[error("text blob was not hydrated ({bytes} bytes)")]
    UnhydratedTextBlob { bytes: u64 },
}

fn fence(body: &str, language: &str) -> String {
    let mut longest_run = 0;
    let mut current_run = 0;
    for byte in body.bytes() {
        if byte == b'`' {
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    let fence_len = longest_run.max(2) + 1;
    let mut result = String::with_capacity(fence_len * 2 + language.len() + body.len() + 2);
    result.extend(std::iter::repeat_n('`', fence_len));
    result.push_str(language);
    result.push('\n');
    result.push_str(body);
    if !body.ends_with('\n') {
        result.push('\n');
    }
    result.extend(std::iter::repeat_n('`', fence_len));
    result
}

fn append_section(output: &mut String, section: &str) {
    output.push_str("\n\n");
    output.push_str(section);
}

fn append_summary(output: &mut String, summary: Option<&str>) {
    let mut section = String::from("## Summary of earlier turns");
    if let Some(summary) = summary {
        section.push_str("\n\n");
        section.push_str(summary);
    }
    append_section(output, &section);
}

fn assistant_section(content: &[Block]) -> Option<String> {
    let mut section = None;
    for block in content {
        match block {
            Block::Text { text } => {
                let section = section.get_or_insert_with(|| String::from("## dalgon"));
                section.push_str("\n\n");
                section.push_str(text);
            }
            Block::ToolCall { name, input, .. } => {
                let section = section.get_or_insert_with(|| String::from("## dalgon"));
                let _ = write!(
                    section,
                    "\n\n### Tool call: {name}\n\n{}",
                    fence(input.as_str(), "json")
                );
            }
            Block::Reasoning { .. } => {}
        }
    }
    section
}

fn render_parts(parts: &[JournalPart]) -> Result<String, MarkdownError> {
    let mut body = String::new();
    let mut has_part = false;
    for part in parts {
        if has_part {
            body.push_str("\n\n");
        }
        has_part = true;
        match part {
            JournalPart::Text { text } => body.push_str(text),
            JournalPart::TextBlob { bytes, .. } => {
                return Err(MarkdownError::UnhydratedTextBlob { bytes: *bytes });
            }
            JournalPart::Image { mime, base64 } => {
                let _ = write!(body, "[image: {mime}, {} bytes]", base64_bytes_len(base64));
            }
            JournalPart::ImageBlob { mime, bytes, .. } => {
                let _ = write!(body, "[image: {mime}, {bytes} bytes]");
            }
            JournalPart::Blob { mime, bytes, .. } => {
                let _ = write!(body, "[blob: {mime}, {bytes} bytes]");
            }
        }
    }
    Ok(body)
}

fn base64_bytes_len(encoded: &str) -> usize {
    let padding = encoded
        .as_bytes()
        .iter()
        .rev()
        .take(2)
        .take_while(|byte| **byte == b'=')
        .count();
    (encoded.len() / 4 * 3 + encoded.len() % 4 * 3 / 4).saturating_sub(padding)
}

fn render_jsonl(entries: &[EntryView]) -> Result<String, sonic_rs::Error> {
    let mut out = String::new();
    for entry in entries {
        out.push_str(&sonic_rs::to_string(entry)?);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
