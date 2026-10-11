use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use crate::session::tasks::SessionTasks;
use dal_core::command::{Command, ExportFormat};
use dal_core::{
    ApprovalMode, AssistantStop, AutoCompaction, Block, CallId, EntryId, EntryKind, EntryView,
    Family, Gen, JournalPart, Mode, Origin, RawJson, Seq, SessionId, SessionInfo, SettingsView,
    Stats, ThinkingLevel, TreeOutline, TurnState, Usage, UsageView, View, Workspace,
};

#[cfg(unix)]
use super::relative_export_path_escapes_workspace;
use super::{
    ExportWriteError, MarkdownError, TempFileCleanup, base64_bytes_len, export_success_message,
    export_target, export_write_error, fence, render_markdown, render_parts,
    untrusted_export_path_denied, write_export,
};

async fn assert_no_export_temp_files(target: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let parent = target.parent().ok_or("target parent is missing")?;
    let name = target.file_name().ok_or("target file name is missing")?;
    let prefix = format!(".{}.dalgon-{}-", name.to_string_lossy(), std::process::id());
    let mut entries = tokio::fs::read_dir(parent).await?;
    while let Some(entry) = entries.next_entry().await? {
        let temp_name = entry.file_name();
        let temp_name = temp_name.to_string_lossy();
        let is_export_temp = temp_name.starts_with(prefix.as_str()) && temp_name.ends_with(".tmp");
        assert!(
            !is_export_temp,
            "export temporary file remains: {temp_name}"
        );
    }
    Ok(())
}

fn usage() -> Usage {
    Usage {
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("fixture id is nonzero"))
}

fn view(workspace: Workspace) -> View {
    View {
        r#gen: Gen::new(NonZeroU64::MIN),
        seq: Seq::new(NonZeroU64::MIN),
        session: SessionInfo {
            id: SessionId::parse("018f0f62-3b00-7000-8000-000000000001")
                .expect("fixture session id is valid"),
            name: Some("Moon".into()),
            preview: "Hello".into(),
            workspace,
            updated_at: jiff::Timestamp::UNIX_EPOCH,
            created_at: Some(jiff::Timestamp::UNIX_EPOCH),
            archived: Some(false),
            last_seq: Some(Seq::new(NonZeroU64::MIN)),
        },
        turn: TurnState::Idle,
        entries: dal_core::Page {
            items: Vec::new(),
            next_before: None,
        },
        tree: TreeOutline {
            branches: Vec::new(),
        },
        settings: SettingsView {
            model: None,
            thinking: ThinkingLevel::Medium,
            approval: ApprovalMode::Ask,
            mode: Mode::Normal,
            name: Some("Moon".into()),
        },
        open: Vec::new(),
        changes: Vec::new(),
        usage: UsageView {
            usage: usage(),
            context_tokens: 0,
            context_window: 128_000,
        },
        stats: Stats {
            steers_queued: 0,
            follow_ups_queued: 0,
            retries: 0,
            dropped_observations: 0,
            auto_compaction: AutoCompaction::Off,
        },
    }
}

#[test]
fn markdown_export_includes_assistant_reply() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let workspace = Workspace::new(root.path().to_path_buf())?;
    let view = view(workspace);
    let entries = [
        EntryView {
            id: entry_id(1),
            parent: None,
            kind: EntryKind::User {
                parts: vec![
                    JournalPart::Text {
                        text: "Hello".into(),
                    },
                    JournalPart::Image {
                        mime: "image/png".into(),
                        base64: "AQID".into(),
                    },
                ],
            },
        },
        EntryView {
            id: entry_id(2),
            parent: Some(entry_id(1)),
            kind: EntryKind::Assistant {
                api: Family::Responses,
                model: "gpt-test".into(),
                content: vec![
                    Block::Reasoning {
                        text: "private reasoning".into(),
                        replay: RawJson::null(),
                    },
                    Block::Text {
                        text: "Answer".into(),
                    },
                    Block::ToolCall {
                        id: CallId::new("call-1"),
                        name: "read".into(),
                        input: RawJson::parse(r#"{"path":"src"}"#)?,
                    },
                ],
                usage: usage(),
                stop: AssistantStop::Done,
            },
        },
        EntryView {
            id: entry_id(3),
            parent: Some(entry_id(2)),
            kind: EntryKind::ToolResult {
                call: CallId::new("call-1"),
                name: "read".into(),
                error: false,
                parts: vec![JournalPart::Text {
                    text: "done".into(),
                }],
                changes: Vec::new(),
            },
        },
        EntryView {
            id: entry_id(4),
            parent: Some(entry_id(3)),
            kind: EntryKind::Compaction {
                summary: Some("Older turns summarized".into()),
                first_kept: None,
                tokens_before: 100,
                replay: None,
                usage: None,
                parts: Vec::new(),
                parts_tokens: 0,
            },
        },
    ];
    let at = jiff::Timestamp::UNIX_EPOCH.to_zoned(jiff::tz::TimeZone::UTC);

    let expected = format!(
        concat!(
            "# Moon\n\n",
            "Session 018f0f62-3b00-7000-8000-000000000001 · ",
            "exported 1970-01-01 00:00 · {}\n\n",
            "## You\n\nHello\n\n[image: image/png, 3 bytes]\n\n",
            "## dalgon\n\nAnswer\n\n### Tool call: read\n\n",
            "```json\n{{\"path\":\"src\"}}\n```\n\n",
            "### Tool result: read · ok\n\n```text\ndone\n```\n\n",
            "## Summary of earlier turns\n\nOlder turns summarized\n",
        ),
        view.session.workspace.as_path().display()
    );
    assert_eq!(render_markdown(&view, &entries, &at)?, expected);
    Ok(())
}

#[test]
fn markdown_fences_exceed_embedded_backtick_runs() {
    assert_eq!(fence("a```b", "json"), "````json\na```b\n````");
}

#[test]
fn base64_image_length_subtracts_padding() {
    assert_eq!(base64_bytes_len("AQIDBA=="), 4);
}

#[test]
fn unhydrated_text_blob_is_rejected() {
    let parts = [JournalPart::TextBlob {
        blob: "0".repeat(64).into(),
        bytes: 7,
    }];

    assert_eq!(
        render_parts(&parts),
        Err(MarkdownError::UnhydratedTextBlob { bytes: 7 })
    );
}

#[test]
fn relative_export_path_resolves_against_workspace() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let workspace = Workspace::new(root.path().join("workspace"))?;

    assert_eq!(
        export_target(
            &workspace,
            Some(PathBuf::from("nested/session.md")),
            ExportFormat::Markdown,
        ),
        workspace.as_path().join("nested/session.md")
    );
    assert_eq!(
        export_target(
            &workspace,
            Some(PathBuf::from("../outside.md")),
            ExportFormat::Markdown,
        ),
        workspace.as_path().join("../outside.md")
    );
    Ok(())
}

#[test]
fn export_success_message_reports_the_absolute_target() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let target = root.path().join("session.md");

    assert_eq!(
        export_success_message(&target),
        format!("Exported the session to {}.", target.display())
    );
    Ok(())
}

#[test]
fn directory_sync_error_reports_the_published_target() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let target = root.path().join("session.md");
    let error = export_write_error(
        &target,
        ExportWriteError::PublishedButNotDurable(std::io::Error::other("sync denied")),
    )
    .to_string();

    assert!(error.contains("export target was published to"));
    assert!(error.contains(&target.display().to_string()));
    assert!(error.contains("directory sync failed: sync denied"));
    Ok(())
}

#[tokio::test]
async fn temp_cleanup_is_drained_before_session_shutdown() -> Result<(), Box<dyn std::error::Error>>
{
    let root = tempfile::tempdir()?;
    let path = root.path().join("partial.tmp");
    tokio::fs::write(&path, b"partial export").await?;
    let tasks = SessionTasks::new();
    drop(TempFileCleanup::new(
        path.clone(),
        tasks.clone(),
        tokio::runtime::Handle::current(),
    ));

    tasks.stop().await;

    assert!(!tokio::fs::try_exists(&path).await?);
    Ok(())
}

#[tokio::test]
async fn absolute_export_path_writes_outside_workspace() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let workspace_path = root.path().join("workspace");
    tokio::fs::create_dir(&workspace_path).await?;
    let workspace = Workspace::new(workspace_path)?;
    let target = root.path().join("outside.md");
    let target = export_target(&workspace, Some(target.clone()), ExportFormat::Markdown);
    let tasks = SessionTasks::new();

    write_export(&tasks, &target, b"exported assistant reply").await?;

    assert_eq!(tokio::fs::read(&target).await?, b"exported assistant reply");
    assert_no_export_temp_files(&target).await?;
    Ok(())
}

#[tokio::test]
async fn existing_export_target_is_not_replaced() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let target = root.path().join("session.md");
    tokio::fs::write(&target, b"original").await?;
    let tasks = SessionTasks::new();

    let error = write_export(&tasks, &target, b"replacement")
        .await
        .expect_err("existing target must not be replaced");
    let user_error = export_write_error(&target, error);

    assert!(
        user_error
            .to_string()
            .contains("export target already exists")
    );
    assert_eq!(tokio::fs::read(&target).await?, b"original");
    assert_no_export_temp_files(&target).await?;
    Ok(())
}

#[tokio::test]
async fn concurrent_exports_publish_exactly_one_target() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let target = root.path().join("session.md");
    let tasks = SessionTasks::new();
    let (left, right) = tokio::join!(
        write_export(&tasks, &target, b"left export"),
        write_export(&tasks, &target, b"right export"),
    );

    assert_ne!(left.is_ok(), right.is_ok());
    let error = left.err().or_else(|| right.err());
    assert!(matches!(error, Some(ExportWriteError::TargetExists)));
    let content = tokio::fs::read(&target).await?;
    let left: &[u8] = b"left export";
    let right: &[u8] = b"right export";
    assert!(content.as_slice() == left || content.as_slice() == right);
    assert_no_export_temp_files(&target).await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn relative_export_paths_cannot_follow_workspace_symlinks()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let workspace_path = root.path().join("workspace");
    let outside = root.path().join("outside");
    tokio::fs::create_dir(&workspace_path).await?;
    tokio::fs::create_dir(&outside).await?;
    std::os::unix::fs::symlink(&outside, workspace_path.join("link"))?;
    let workspace = Workspace::new(workspace_path)?;
    let symlink_target = workspace.as_path().join("link/new/session.md");
    let nested_target = workspace.as_path().join("nested/session.md");

    assert!(relative_export_path_escapes_workspace(&workspace, &symlink_target).await);
    assert!(!relative_export_path_escapes_workspace(&workspace, &nested_target).await);
    Ok(())
}

#[test]
fn only_builtin_callers_can_schedule_absolute_export_targets()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let absolute = Command::Export {
        path: Some(root.path().join("outside/session.md")),
        format: ExportFormat::Markdown,
    };
    let relative = Command::Export {
        path: Some(PathBuf::from("session.md")),
        format: ExportFormat::Markdown,
    };
    let traversal = Command::Export {
        path: Some(PathBuf::from("../outside.md")),
        format: ExportFormat::Markdown,
    };

    assert!(untrusted_export_path_denied(Origin::User, &absolute));
    assert!(untrusted_export_path_denied(Origin::Bundled, &absolute));
    assert!(!untrusted_export_path_denied(Origin::Builtin, &absolute));
    assert!(!untrusted_export_path_denied(Origin::User, &relative));
    assert!(untrusted_export_path_denied(Origin::User, &traversal));
    assert!(!untrusted_export_path_denied(Origin::Builtin, &traversal));
    Ok(())
}
