//! Session-level commands and export formatting.
use dal_agent::ext::command::CommandCx;
use dal_core::command::{Command, ErrorTriple, FrontAction, Output, Reply};
use dal_core::{ApprovalMode, Block, EntryKind, ModelRoute};

/// Runs `/export`: picks the format, resolves the target, and starts the job.
///
/// The job body renders and writes the selected format; this handler selects
/// the format, default name, and target path.
///
/// # Errors
///
/// Returns the extension pair for anything but `.md`/`.jsonl`, and the
/// empty-session pair when there is nothing to write.
pub(super) fn export(cx: &CommandCx<'_>, path: Option<&str>) -> Result<Reply, ErrorTriple> {
    if cx.leaf_entries().is_empty() {
        return Err(nothing_to_export());
    }
    let workspace = cx.view().session.workspace.clone();
    let (target, format) = if let Some(raw) = path {
        let trimmed = raw.trim();
        let file = std::path::Path::new(trimmed);
        let ext = file
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let format = export_format(&ext)?;
        let absolute = resolve_in(&workspace, file);
        (absolute, format)
    } else {
        let name = cx.view().settings.name.clone();
        let id = cx.view().session.id.to_string();
        let target = default_export_path(&workspace, name.as_deref(), &id, &jiff::Zoned::now());
        (target, dal_core::command::ExportFormat::Markdown)
    };
    Ok(Reply::Started(cx.start_job(Command::Export {
        path: Some(target),
        format,
    })))
}

/// Picks the export format from a lowercase file extension.
///
/// # Errors
///
/// Returns the extension pair for anything but `md` and `jsonl`.
pub(super) fn export_format(ext: &str) -> Result<dal_core::command::ExportFormat, ErrorTriple> {
    match ext {
        "md" => Ok(dal_core::command::ExportFormat::Markdown),
        "jsonl" => Ok(dal_core::command::ExportFormat::Jsonl),
        _ => Err(super::error_triple(
            format!("dalgon cannot export \"{ext}\" files"),
            "export writes .md or .jsonl",
            "Type /export PATH.md or /export PATH.jsonl.",
        )),
    }
}

/// The empty-session pair for `/export` with nothing to write.
pub(super) fn nothing_to_export() -> ErrorTriple {
    super::error_triple(
        "There is nothing to export",
        "the session has no messages",
        "Send a message first.",
    )
}

/// Builds the default export target below the session workspace.
pub(super) fn default_export_path(
    workspace: &dal_core::Workspace,
    name: Option<&str>,
    id: &str,
    at: &jiff::Zoned,
) -> std::path::PathBuf {
    let slug = slug(name.unwrap_or(""), id);
    let stamp = at.strftime("%Y%m%d-%H%M%S").to_string();
    let file = format!("dalgon-{slug}-{stamp}.md");
    resolve_in(workspace, std::path::Path::new(&file))
}

/// Resolves an export or import path against the session workspace.
///
/// Absolute paths pass through; anything else joins to the workspace root,
/// never the process working directory.
pub(super) fn resolve_in(
    workspace: &dal_core::Workspace,
    file: &std::path::Path,
) -> std::path::PathBuf {
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        workspace.as_path().join(file)
    }
}

/// Runs `/import`: resolves the path and returns the client-side import action.
///
/// The handler touches no file; the client validates the codec and the
/// 16 MiB line cap, and its typed `ImportFailure` renders through
/// `render_import`.
///
/// # Errors
///
/// Returns the no-path pair for an empty argument.
pub(super) fn import(cx: &CommandCx<'_>, raw: &str) -> Result<Reply, ErrorTriple> {
    if raw.trim().is_empty() {
        return Err(super::error_triple(
            "/import needs a path",
            "no file was given",
            "Type /import PATH.jsonl with a file written by /export.",
        ));
    }
    let workspace = cx.view().session.workspace.clone();
    let absolute = resolve_in(&workspace, std::path::Path::new(raw.trim()));
    Ok(Reply::Front(FrontAction::Import { path: absolute }))
}

/// Runs `/copy`: copies the last assistant reply through OSC 52.
///
/// Takes the text blocks of the last assistant message on the leaf path,
/// joined with one blank line; images and reasoning never reach the
/// clipboard. The terminal owns the ack line.
///
/// # Errors
///
/// Returns the no-reply pair with no assistant message, and the too-large
/// pair when the base64 form passes the OSC 52 ceiling.
pub(super) fn copy(cx: &CommandCx<'_>) -> Result<Reply, ErrorTriple> {
    let mut reply: Option<String> = None;
    for entry in cx.leaf_entries() {
        if let EntryKind::Assistant { content, .. } = &entry.kind {
            let texts: Vec<&str> = content
                .iter()
                .filter_map(|block| match block {
                    Block::Text { text } => Some(&**text),
                    Block::Reasoning { .. } | Block::ToolCall { .. } => None,
                })
                .collect();
            if !texts.is_empty() {
                reply = Some(texts.join("\n\n"));
            }
        }
    }
    let Some(text) = reply else {
        return Err(no_reply());
    };
    copy_limit_check(u64::try_from(text.len()).unwrap_or(u64::MAX))?;
    Ok(Reply::Front(FrontAction::CopyReply { text: text.into() }))
}

/// The no-reply pair for `/copy` with no assistant message yet.
pub(super) fn no_reply() -> ErrorTriple {
    super::error_triple(
        "There is no reply to copy",
        "the session has no assistant message yet",
        "Send a message first.",
    )
}

/// Rejects a reply whose base64 form passes the OSC 52 ceiling.
///
/// # Errors
///
/// Returns the too-large pair when `4 * ceil(bytes / 3)` exceeds the limit.
pub(super) fn copy_limit_check(bytes: u64) -> Result<(), ErrorTriple> {
    let encoded = bytes
        .checked_add(2)
        .map(|ended| ended / 3)
        .and_then(|groups| groups.checked_mul(4));
    if encoded.is_none_or(|length| length > COPY_BYTE_LIMIT) {
        return Err(super::error_triple(
            format!(
                "The last reply is too large to copy: it has {bytes} bytes and OSC 52 carries at most {COPY_BYTE_LIMIT}"
            ),
            "the clipboard path cannot carry it",
            "Type /export to write the session to a file.",
        ));
    }
    Ok(())
}

/// The largest base64 reply the OSC 52 clipboard path carries.
const COPY_BYTE_LIMIT: u64 = 6_291_456;

/// Runs `/name`: shows the session name or sets it through `Rename`.
///
/// # Errors
///
/// Returns the too-long and control-character pairs for invalid names.
pub(super) async fn name(cx: &CommandCx<'_>, raw: &str) -> Result<Reply, ErrorTriple> {
    let word = raw.trim();
    if word.is_empty() {
        let line = match cx.view().settings.name.as_deref() {
            Some(name) => format!("Session name: {name}"),
            None => "This session has no name. Type /name NAME to set one.".to_owned(),
        };
        return Ok(Reply::Done(Output::Text(line.into())));
    }
    let quoted = check_name(word)?;
    let _ = cx.submit_wait(Command::Rename(word.into())).await;
    Ok(Reply::Done(Output::Text(
        format!("Session name: {word}. Resume it with dalgon -r {quoted}.").into(),
    )))
}

/// Validates a `/name` argument and quotes it for the resume hint.
///
/// Returns the name quoted in double quotes when any scalar falls outside
/// `[A-Za-z0-9._-]`.
///
/// # Errors
///
/// Returns the too-long pair past 64 Unicode scalars and the
/// control-character pair for `U+0000` to `U+001F` and `U+007F`.
pub(super) fn check_name(word: &str) -> Result<String, ErrorTriple> {
    let length = u64::try_from(word.chars().count()).unwrap_or(u64::MAX);
    if length > 64 {
        return Err(super::error_triple(
            format!("The name is too long: it has {length} characters and the limit is 64"),
            "names hold at most 64 characters",
            "Type a shorter name.",
        ));
    }
    if word
        .chars()
        .any(|scalar| matches!(scalar, '\u{0}'..='\u{1f}' | '\u{7f}'))
    {
        return Err(super::error_triple(
            "The name holds a control character",
            "names must be printable text",
            "Type the name again without tabs or line breaks.",
        ));
    }
    Ok(
        if word
            .chars()
            .all(|scalar| scalar.is_ascii_alphanumeric() || matches!(scalar, '.' | '_' | '-'))
        {
            word.to_owned()
        } else {
            format!("\"{word}\"")
        },
    )
}

/// Runs `/session`: returns the session details table from one snapshot.
///
/// Labels arrive in plan order; the first cell of each row holds the label.
/// Counts cover the host-truncated leaf entries. No failure path.
pub(super) fn details(cx: &CommandCx<'_>) -> Reply {
    let view = cx.view();
    let settings = &view.settings;
    let mut rows: Vec<Vec<Box<str>>> = Vec::with_capacity(14);
    let mut row = |label: &str, value: String| {
        rows.push(vec![label.into(), value.into()]);
    };
    row(
        "Session",
        settings.name.as_deref().unwrap_or("no name").to_owned(),
    );
    row("Id", view.session.id.to_string());
    row(
        "File",
        cx.session_file().map_or_else(
            || "not saved (--no-session)".to_owned(),
            |path| path.display().to_string(),
        ),
    );
    row(
        "Workspace",
        view.session.workspace.as_path().display().to_string(),
    );
    row(
        "Model",
        settings
            .model
            .as_ref()
            .map_or("no model", ModelRoute::id)
            .to_owned(),
    );
    row("Mode", settings.mode.as_str().to_owned());
    row("Thinking", settings.thinking.name().to_owned());
    row(
        "Approval",
        match settings.approval {
            ApprovalMode::Ask => "ask before patch and exec",
            ApprovalMode::Edits => "ask before exec",
            ApprovalMode::All => "never ask",
        }
        .to_owned(),
    );
    let mut you = 0_u64;
    let mut dalgon = 0_u64;
    let mut tools = 0_u64;
    let mut calls = 0_u64;
    for entry in cx.leaf_entries() {
        match &entry.kind {
            EntryKind::User { .. } => you += 1,
            EntryKind::Assistant { content, .. } => {
                dalgon += 1;
                calls += content.iter().fold(0_u64, |count, block| {
                    count + u64::from(matches!(block, Block::ToolCall { .. }))
                });
            }
            EntryKind::ToolResult { .. } => tools += 1,
            _ => {}
        }
    }
    row(
        "Messages",
        format!(
            "{} ({you} from you, {dalgon} from dalgon, {tools} tool result{})",
            you + dalgon + tools,
            super::plural(tools),
        ),
    );
    row("Tool calls", format!("{calls}"));
    let usage = &view.usage.usage;
    row(
        "Tokens",
        format!(
            "in {} (cached {}) out {}",
            super::comma_group(usage.input_tokens),
            super::comma_group(usage.cached_input_tokens),
            super::comma_group(usage.output_tokens),
        ),
    );
    row(
        "Cost",
        match usage.cost_usd {
            Some(cost) if cost > 0.0 => format!("${cost:.3}"),
            _ => "unknown for this model".to_owned(),
        },
    );
    let files = u64::try_from(view.changes.len()).unwrap_or(u64::MAX);
    row(
        "Files changed",
        format!("{files} file{}", super::plural(files)),
    );
    row("Log", cx.log_path().display().to_string());
    Reply::Done(Output::Table(rows))
}

/// Runs `/changelog`: returns the embedded changelog page as Markdown.
pub(super) fn changelog(cx: &CommandCx<'_>) -> Reply {
    Reply::Done(Output::Markdown(cx.docs_page(cx.changelog_uri())))
}

/// Runs `/hotkeys`: returns the client-side shortcut catalog action.
pub(super) fn hotkeys() -> Reply {
    Reply::Front(FrontAction::ShowKeys)
}

/// Runs `/new`: returns the client-side new-session action.
pub(super) fn new_session() -> Reply {
    Reply::Front(FrontAction::NewSession)
}

/// Runs `/resume`: opens the picker or switches to one resolved session.
///
/// Resolution tries an id prefix first, then a name, across workspace pages.
///
/// # Errors
///
/// Returns the no-match pair when nothing on the pages matches.
pub(super) fn resume(cx: &CommandCx<'_>, raw: &str) -> Result<Reply, ErrorTriple> {
    let word = raw.trim();
    if word.is_empty() {
        return Ok(Reply::Choose {
            chooser: dal_core::command::Chooser::Session,
            filter: "".into(),
        });
    }
    match cx.resolve_session(word) {
        Ok(summary) => {
            if summary.id == cx.session() {
                return Ok(Reply::Done(Output::Text(
                    "This is the current session.".into(),
                )));
            }
            Ok(Reply::Front(FrontAction::Resume { session: summary }))
        }
        Err(_) => Err(no_session(word)),
    }
}

/// The no-match pair for `/resume` when the lookup misses.
pub(super) fn no_session(word: &str) -> ErrorTriple {
    super::error_triple(
        format!("No session named \"{word}\" in this workspace"),
        "dalgon looked for an id or a name",
        "Type /resume to pick from the list.",
    )
}

/// Runs `/quit`: cancels the running turn, then returns the quit action.
pub(super) fn quit(cx: &CommandCx<'_>) -> Reply {
    if cx.turn().is_some() {
        cx.cancel_turn();
    }
    Reply::Front(FrontAction::Quit)
}

pub(super) fn slug(name: &str, id: &str) -> String {
    let mut result = String::with_capacity(name.len().min(40));
    let mut previous_hyphen = true;
    for character in name.chars() {
        let character = if character.is_ascii_lowercase() || character.is_ascii_digit() {
            character
        } else {
            '-'
        };
        if character == '-' {
            if previous_hyphen {
                continue;
            }
            previous_hyphen = true;
        } else {
            previous_hyphen = false;
        }
        if result.len() == 40 {
            break;
        }
        result.push(character);
    }
    while result.ends_with('-') {
        result.pop();
    }
    if result.is_empty() {
        super::id8(id)
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::slug;

    #[test]
    fn slug_collapses_separators_and_replaces_other_scalars() {
        assert_eq!(slug("a---b", "identifier"), "a-b");
        assert_eq!(slug("  My—Session! ", "identifier"), "y-ession");
    }

    #[test]
    fn slug_uses_identifier_when_name_has_no_ascii_alphanumeric() {
        assert_eq!(slug("!!!", "abcdefghijk"), "abcdefgh");
    }

    #[test]
    fn slug_is_limited_to_forty_characters() {
        assert_eq!(slug(&"a".repeat(50), "identifier"), "a".repeat(40));
    }
}
