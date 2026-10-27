//! Tool-boundary proofs: read, search, patch, and exec obligations.
//!
//! Every test drives the registered `dal-tools` extension through the public
//! `Tool` seam with a deny/approve test context, exactly as a host would:
//! classify-then-run, model-visible error strings, and real filesystem bytes
//! under a scratch directory inside the test workspace (`/tmp`). Nothing here
//! reaches into `pub(crate)` engine internals; a green suite proves the
//! boundary contracts a model actually meets.
//!
//! Obligation map:
//! - RST-02, RST-12, patch containment, ETP-04: path authorization tests.
//! - PTS-04: stale-input / proof tests (whole-tag round trip, no-tag refusal).
//! - PTS-06/08: atomic-edit tests (multi-file and overlap failures write nothing).
//! - PTS-07: mode tests (update preservation, create narrowing).
//! - RST-04/05, RST-07/10: bounded-output tests (window cap, line cut, dir cap, limits).
//! - RST-08/09: index-freshness tests (patch dirties the shared index; no stale hits).
//! - ETP-02/03: classifier and argument-bound tests.
//!
//! Fixture rules: every fixture lives in its own `tempdir_in(workspace)`
//! scratch directory, so parallel tests never share bytes. Outside-workspace
//! probes use an existing system file outside the workspace (`outside_file`),
//! never a created fixture: nothing the tests write can land off-workspace.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tool boundary tests fail loudly at their setup boundary"
)]

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use dal_agent::ext::services::ServiceFuture;
use dal_agent::ext::{
    Caller, EventStream, Extension, RawValue, Services, Tool, ToolCall, ToolCx, ToolOutcome,
};
use dal_core::ext::{McpDeclaration, McpRequest, McpResponse};
use dal_core::{
    AgentsOp, AgentsReply, Answer, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
    JobsReply, ModelRequest, Notice, Part, Question, RunOutput, RunRequest, SidecarOp, StateError,
    StateOp, StateRecord, ToolClass, TurnOp, TurnOpReply, Visibility,
};

/// Services the tools under test never touch: read/search/patch/exec run
/// against the filesystem and the in-extension index, not host services.
struct DeadServices;

impl Services for DeadServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unreachable!("tool boundary tests do not use services")
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unreachable!("tool boundary tests do not use services")
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unreachable!("tool boundary tests do not use services")
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unreachable!("tool boundary tests do not use services")
    }

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        unreachable!("tool boundary tests do not use services")
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<dal_core::PrivateEffect>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        unreachable!("tool boundary tests do not use services")
    }

    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        unreachable!("tool boundary tests do not use services")
    }

    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unreachable!("tool boundary tests do not use services")
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<dal_agent::ext::Doc>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unreachable!("tool boundary tests do not use services")
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn state(
        &self,
        _who: &Caller,
        _op: StateOp,
    ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn model_ready(
        &self,
        _who: &Caller,
        _model: dal_core::ModelRoute,
    ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unreachable!("tool boundary tests do not use services")
    }

    fn infer_stream(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        unreachable!("tool boundary tests do not use services")
    }

    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unreachable!("tool boundary tests do not use services")
    }

    fn notify(&self, _who: &Caller, _notice: Notice) {
        unreachable!("tool boundary tests do not use services")
    }

    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unreachable!("tool boundary tests do not use services")
    }

    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        unreachable!("tool boundary tests do not use services")
    }

    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unreachable!("tool boundary tests do not use services")
    }

    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("tool boundary tests do not use services")
    }
}

fn svc() -> Arc<DeadServices> {
    Arc::new(DeadServices)
}

/// Builds the real tools extension; `index_root` enables the persistent
/// trigram index (freshness tests) or disables it (everything else, where the
/// full-scan fallback keeps each test hermetic).
///
/// The edit style is pinned to `replace` so the structured `{"changes": …}`
/// envelope parses the same way in every test; freeform dialects keep their
/// own in-crate coverage.
fn extension_with(index_root: Option<PathBuf>) -> Extension {
    let config = dal_tools::ToolsConfig {
        index_root,
        edit_style: dal_core::EditStyleInput::Scalar("replace".into()),
        ..Default::default()
    };
    dal_tools::extension(config).expect("tools extension registers")
}

fn tool_named(extension: &Extension, name: &str) -> Arc<dyn Tool> {
    extension
        .tools()
        .iter()
        .find(|(tool, _)| tool.name().as_str() == name)
        .map(|(tool, _)| Arc::clone(tool))
        .expect("tool is registered")
}

/// Runs one tool call the way the host does. Each call mints a fresh test
/// context, so `Seen` coverage never leaks between calls: tag-carrying
/// assertions below hold across sessions on purpose.
async fn run(tool: &Arc<dyn Tool>, args: &str) -> ToolOutcome {
    drive(tool, args, ToolCx::for_test(svc())).await
}

/// Runs one approval-gated call with the ladder pre-approved.
async fn run_approved(tool: &Arc<dyn Tool>, args: &str) -> ToolOutcome {
    drive(tool, args, ToolCx::for_test_approved(svc())).await
}

async fn drive(tool: &Arc<dyn Tool>, args: &str, cx: ToolCx<'static>) -> ToolOutcome {
    let raw = RawValue::parse(args).expect("test args are one JSON value");
    tool.run(ToolCall::new("probe-call", raw), cx).await
}

/// Concatenated text parts of a successful outcome; any other outcome fails
/// the test with its own debug shape.
fn ok_text(outcome: ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Ok(output) => output
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } => Some(text.to_string()),
                Part::Image { .. } | Part::Blob { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        ToolOutcome::Err(error) => panic!("expected Ok, got Err: {error}"),
        ToolOutcome::Interrupted => panic!("expected Ok, got Interrupted"),
        ToolOutcome::Detached(_) => panic!("expected Ok, got Detached"),
    }
}

/// Display text of an error outcome; successes fail the test.
fn err_text(outcome: ToolOutcome) -> String {
    match outcome {
        ToolOutcome::Err(error) => error.to_string(),
        ToolOutcome::Ok(output) => panic!("expected Err, got Ok: {output:?}"),
        ToolOutcome::Interrupted => panic!("expected Err, got Interrupted"),
        ToolOutcome::Detached(_) => panic!("expected Err, got Detached"),
    }
}

fn workspace_root() -> PathBuf {
    ToolCx::for_test(svc()).workspace().as_path().to_path_buf()
}

/// A scratch directory directly under the tool workspace plus its
/// workspace-relative spelling, e.g. `.tmpAbC123`.
fn scratch() -> (tempfile::TempDir, String) {
    let root = workspace_root();
    let dir = tempfile::tempdir_in(&root).expect("scratch dir under the workspace");
    let rel = dir
        .path()
        .strip_prefix(&root)
        .expect("scratch is a workspace child")
        .to_string_lossy()
        .into_owned();
    (dir, rel)
}

/// An existing file outside the tool workspace, for escape probes that must
/// not create fixtures outside the workspace. The test workspace is the
/// platform temp dir, so any fixed system path outside it qualifies; the
/// first existing candidate wins.
fn outside_file() -> PathBuf {
    for candidate in ["/etc/hostname", "/etc/hosts", "/"] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return path;
        }
    }
    panic!("no outside-workspace probe path exists")
}

/// Parses the whole-file tag off a complete read: the trailing `[path#TAG]` line.
fn whole_tag(text: &str) -> String {
    let last = text.rsplit('\n').next().expect("read has a last line");
    assert!(
        last.starts_with('[') && last.ends_with(']'),
        "whole read ends with a tag line, got: {last}"
    );
    let inner = &last[1..last.len() - 1];
    let (_, tag) = inner.rsplit_once('#').expect("tag separator");
    tag.to_owned()
}

fn changes(args: &sonic_rs::Value) -> String {
    sonic_rs::to_string(args).expect("changes encode")
}

// ---------------------------------------------------------------------------
// RST-02: read path containment.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_rejects_absolute_and_parent_escape_paths() {
    let (dir, rel) = scratch();
    std::fs::write(dir.path().join("inside.txt"), "inside marker\n").expect("seed inside fixture");
    let extension = extension_with(None);
    let read = tool_named(&extension, "read");

    let absolute_inside = dir.path().join("inside.txt").to_string_lossy().into_owned();
    for bad in [
        absolute_inside,
        String::from("/etc/hostname"),
        format!("../{rel}/inside.txt"),
        format!("{rel}/../../{rel}/inside.txt"),
    ] {
        let args = changes(&sonic_rs::json!({ "path": bad }));
        let error = err_text(run(&read, &args).await);
        assert_eq!(
            error, "read: path must stay within the workspace",
            "refused path: {bad}"
        );
    }

    // Control: the same bytes through a workspace-relative path read fine.
    let args = changes(&sonic_rs::json!({ "path": format!("{rel}/inside.txt") }));
    assert!(
        ok_text(run(&read, &args).await).contains("inside marker"),
        "relative read works"
    );
    assert_eq!(
        std::fs::read(dir.path().join("inside.txt")).expect("inside bytes"),
        b"inside marker\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn read_rejects_symlink_escape_outside_the_workspace() {
    let target = outside_file();
    let before = std::fs::read(&target).unwrap_or_default();
    let (dir, rel) = scratch();
    std::fs::write(dir.path().join("inside.txt"), "inside marker\n").expect("inside fixture");
    std::os::unix::fs::symlink(&target, dir.path().join("escape")).expect("escape symlink");
    std::os::unix::fs::symlink(
        dir.path().join("inside.txt"),
        dir.path().join("inside-link"),
    )
    .expect("inside symlink");

    let extension = extension_with(None);
    let read = tool_named(&extension, "read");
    let args = changes(&sonic_rs::json!({ "path": format!("{rel}/escape") }));
    assert_eq!(
        err_text(run(&read, &args).await),
        "read: path must stay within the workspace"
    );
    assert_eq!(
        std::fs::read(&target).unwrap_or_default(),
        before,
        "the probe only resolves; outside bytes stay untouched"
    );

    let args = changes(&sonic_rs::json!({ "path": format!("{rel}/inside-link") }));
    assert!(
        ok_text(run(&read, &args).await).contains("inside marker"),
        "in-workspace links still read"
    );
}

// ---------------------------------------------------------------------------
// RST-04/05: bounded read output.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_window_stops_at_the_output_cap_with_continuation() {
    use std::fmt::Write as _;

    let (dir, rel) = scratch();
    let mut body = String::new();
    for n in 1..=3500 {
        writeln!(body, "line {n}").expect("buffering a test fixture cannot fail");
    }
    std::fs::write(dir.path().join("big.txt"), &body).expect("seed big file");

    let extension = extension_with(None);
    let read = tool_named(&extension, "read");
    let path = format!("{rel}/big.txt");

    let args = changes(&sonic_rs::json!({ "path": path }));
    let first = ok_text(run(&read, &args).await);
    // 1000 short lines fit the 50 KiB window; the footer, not truncation,
    // carries the continuation offset.
    assert!(
        first.len() < 56_000,
        "window stays bounded, got {} bytes",
        first.len()
    );
    let (rows, footer) = first.rsplit_once('\n').expect("footer line");
    assert_eq!(
        footer, "[Showing lines 1-1000 of 3500. Use :1001 to continue.]",
        "exact continuation footer"
    );
    assert!(rows.lines().count() == 1000, "one window holds 1000 rows");

    let args = changes(&sonic_rs::json!({ "path": path, "offset": 1001 }));
    let second = ok_text(run(&read, &args).await);
    assert!(
        second.starts_with("1001\tline 1001\n"),
        "tiling is disjoint, got: {}",
        &second[..32.min(second.len())]
    );
}

#[tokio::test]
async fn read_cuts_long_lines_and_caps_directory_listings() {
    let (dir, rel) = scratch();
    let line = format!("a{}", "é".repeat(2500));
    assert_eq!(line.len(), 5001, "fixture is one 5001-byte line");
    std::fs::write(dir.path().join("long.txt"), &line).expect("seed long line");

    let listed = dir.path().join("tree");
    std::fs::create_dir(&listed).expect("listing dir");
    for index in 0..1005 {
        std::fs::write(listed.join(format!("f{index:04}")), b"").expect("listing entry");
    }

    let extension = extension_with(None);
    let read = tool_named(&extension, "read");

    let args = changes(&sonic_rs::json!({ "path": format!("{rel}/long.txt") }));
    let rendered = ok_text(run(&read, &args).await);
    let first = rendered.lines().next().expect("one rendered row");
    assert!(
        first.starts_with("1\ta"),
        "row keeps its number, got {first:?}"
    );
    assert!(
        first.ends_with("..."),
        "long line is cut, got tail {first:?}"
    );
    assert!(
        first.len() <= 2010,
        "cut line stays near the 2000-byte budget, got {}",
        first.len()
    );

    let args = changes(&sonic_rs::json!({ "path": format!("{rel}/tree") }));
    let listing = ok_text(run(&read, &args).await);
    assert!(
        listing.contains("[Truncated: 5 more entries]"),
        "directory listing caps at 1000, got tail: {:?}",
        listing.lines().last()
    );
}

// ---------------------------------------------------------------------------
// RST-12: search path containment. RST-07/10: bounded search results.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_rejects_paths_outside_the_workspace() {
    let (dir, rel) = scratch();
    std::fs::write(dir.path().join("note.txt"), "a quiet note\n").expect("seed");
    let extension = extension_with(None);
    let search = tool_named(&extension, "search");

    for bad in [
        String::from("/etc"),
        format!("../{rel}"),
        format!("{rel}/../../{rel}"),
    ] {
        let args = changes(&sonic_rs::json!({ "mode": "find", "pattern": "*", "path": bad }));
        assert_eq!(
            err_text(run(&search, &args).await),
            "search: path must stay within the workspace",
            "refused scope: {bad}"
        );
    }

    let args = changes(&sonic_rs::json!({ "mode": "find", "pattern": "*.txt", "path": rel }));
    assert!(
        ok_text(run(&search, &args).await).contains("note.txt"),
        "workspace-relative scope searches fine"
    );
}

#[tokio::test]
async fn search_results_are_bounded_by_limit() {
    use std::fmt::Write as _;

    let (dir, rel) = scratch();
    let mut body = String::new();
    for n in 1..=300 {
        writeln!(body, "grape_bound_token line {n}").expect("buffering a test fixture cannot fail");
    }
    std::fs::write(dir.path().join("many.txt"), &body).expect("seed many-match file");

    let extension = extension_with(None);
    let search = tool_named(&extension, "search");

    let args = changes(&sonic_rs::json!({
        "mode": "grep",
        "pattern": "grape_bound_token",
        "literal": true,
        "path": rel,
    }));
    let text = ok_text(run(&search, &args).await);
    assert!(
        text.contains("[Truncated: 100 more matches]"),
        "grep caps at the default 200 matches, got tail: {:?}",
        text.lines().last()
    );

    for limit in [0, 1001] {
        let args = changes(&sonic_rs::json!({
            "mode": "grep",
            "pattern": "grape_bound_token",
            "literal": true,
            "path": rel,
            "limit": limit,
        }));
        assert_eq!(
            err_text(run(&search, &args).await),
            "search: limit must be between 1 and 1000",
            "limit {limit} is refused"
        );
    }
}

// ---------------------------------------------------------------------------
// Patch containment + atomicity (PTS-06/08).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_rejects_absolute_and_escaping_paths_and_writes_nothing() {
    let target = outside_file();
    let before = std::fs::read(&target).unwrap_or_default();
    let (dir, rel) = scratch();
    std::fs::write(dir.path().join("keep.txt"), "keep\n").expect("inside fixture");

    let extension = extension_with(None);
    let patch_tool = tool_named(&extension, "patch");
    for bad in [
        target.to_string_lossy().into_owned(),
        format!("../{rel}/keep.txt"),
    ] {
        let args = changes(&sonic_rs::json!({ "changes": [{ "path": bad, "create": "evil\n" }] }));
        let text = ok_text(run_approved(&patch_tool, &args).await);
        assert!(
            text.contains("outside the workspace"),
            "containment wording, got: {text}"
        );
    }
    assert_eq!(
        std::fs::read(&target).unwrap_or_default(),
        before,
        "refused patch writes nothing outside"
    );
    assert_eq!(
        std::fs::read(dir.path().join("keep.txt")).expect("inside bytes"),
        b"keep\n"
    );
}

#[tokio::test]
async fn patch_multi_file_failure_writes_nothing() {
    let (dir, rel) = scratch();
    std::fs::write(dir.path().join("a.txt"), "alpha needle omega\n").expect("seed a");
    std::fs::write(dir.path().join("b.txt"), "beta\n").expect("seed b");

    let extension = extension_with(None);
    let patch_tool = tool_named(&extension, "patch");
    // The first change is valid; the second carries a stale tag, so the whole
    // plan must die at staging before anything is published.
    let args = changes(&sonic_rs::json!({ "changes": [
        { "path": format!("{rel}/a.txt"), "old": "needle", "new": "NEW" },
        { "path": format!("{rel}/b.txt"), "tag": "DEADBEEF", "new": "x\n" },
    ] }));
    let text = ok_text(run_approved(&patch_tool, &args).await);
    assert!(
        text.contains("does not match"),
        "stale tag rejects the plan, got: {text}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("a.txt")).expect("a bytes"),
        b"alpha needle omega\n",
        "the valid sibling change is rolled back with the plan"
    );
    assert_eq!(
        std::fs::read(dir.path().join("b.txt")).expect("b bytes"),
        b"beta\n"
    );
}

#[tokio::test]
async fn patch_overlapping_edits_write_nothing() {
    let (dir, rel) = scratch();
    std::fs::write(dir.path().join("o.txt"), "alpha\nneedle\nomega\n").expect("seed");
    let extension = extension_with(None);
    let patch_tool = tool_named(&extension, "patch");
    let args = changes(&sonic_rs::json!({ "changes": [
        { "path": format!("{rel}/o.txt"), "old": "alpha\nneedle", "new": "X" },
        { "path": format!("{rel}/o.txt"), "old": "needle\nomega", "new": "Y" },
    ] }));
    let text = ok_text(run_approved(&patch_tool, &args).await);
    assert!(text.contains("overlap"), "overlap rejects, got: {text}");
    assert_eq!(
        std::fs::read(dir.path().join("o.txt")).expect("o bytes"),
        b"alpha\nneedle\nomega\n",
        "overlapping plan writes nothing"
    );
}

// ---------------------------------------------------------------------------
// PTS-04: stale-input rejection and proof.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_whole_tag_round_trip_then_stale_tag_rejected() {
    let (dir, rel) = scratch();
    let path = format!("{rel}/v.txt");
    std::fs::write(dir.path().join("v.txt"), "v1 line\n").expect("seed v1");

    let extension = extension_with(None);
    let read = tool_named(&extension, "read");
    let patch_tool = tool_named(&extension, "patch");

    let args = changes(&sonic_rs::json!({ "path": path }));
    let tag_v1 = whole_tag(&ok_text(run(&read, &args).await));

    let args = changes(&sonic_rs::json!({ "changes": [
        { "path": path, "tag": tag_v1, "new": "v2 line\n" },
    ] }));
    let text = ok_text(run_approved(&patch_tool, &args).await);
    assert!(
        text.starts_with("Success"),
        "fresh tag applies, got: {text}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("v.txt")).expect("v bytes"),
        b"v2 line\n"
    );

    let args = changes(&sonic_rs::json!({ "path": path }));
    let tag_v2 = whole_tag(&ok_text(run(&read, &args).await));
    assert_ne!(tag_v1, tag_v2, "the tag moves with the bytes");

    // The old tag is now stale input: rejection must name the drift and keep
    // the current bytes.
    let args = changes(&sonic_rs::json!({ "changes": [
        { "path": path, "tag": tag_v1, "new": "v3 line\n" },
    ] }));
    let text = ok_text(run_approved(&patch_tool, &args).await);
    assert!(
        text.contains("does not match"),
        "stale tag rejects, got: {text}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("v.txt")).expect("v bytes"),
        b"v2 line\n",
        "stale write lands nowhere"
    );
}

#[tokio::test]
async fn patch_whole_replace_without_tag_needs_read_proof() {
    let (dir, rel) = scratch();
    std::fs::write(dir.path().join("p.txt"), "hello\n").expect("seed");
    let extension = extension_with(None);
    let patch_tool = tool_named(&extension, "patch");
    // Whole-file replacement without a tag needs Seen proof of the full
    // read; a fresh test context has none, so the plan must refuse.
    let args = changes(&sonic_rs::json!({ "changes": [
        { "path": format!("{rel}/p.txt"), "new": "bye\n" },
    ] }));
    let text = ok_text(run_approved(&patch_tool, &args).await);
    assert!(text.contains("needs tag"), "proof required, got: {text}");
    assert_eq!(
        std::fs::read(dir.path().join("p.txt")).expect("p bytes"),
        b"hello\n",
        "unproven whole replace writes nothing"
    );
}

// ---------------------------------------------------------------------------
// PTS-07: mode preservation (Unix).
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn file_mode(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).expect("mode metadata").mode() & 0o777
}

#[cfg(unix)]
#[tokio::test]
async fn patch_update_preserves_target_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    let (dir, rel) = scratch();
    let plain = dir.path().join("plain.txt");
    let exec = dir.path().join("run.sh");
    std::fs::write(&plain, "plain needle\n").expect("seed plain");
    std::fs::write(&exec, "#!/bin/sh\nneedle\n").expect("seed exec");
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o600)).expect("chmod 0600");
    std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).expect("chmod 0755");

    let extension = extension_with(None);
    let patch_tool = tool_named(&extension, "patch");
    let args = changes(&sonic_rs::json!({ "changes": [
        { "path": format!("{rel}/plain.txt"), "old": "needle", "new": "NEW" },
        { "path": format!("{rel}/run.sh"), "old": "needle", "new": "NEW" },
    ] }));
    let text = ok_text(run_approved(&patch_tool, &args).await);
    assert!(
        text.starts_with("Success"),
        "mode test patch applies, got: {text}"
    );
    assert_eq!(std::fs::read(&plain).expect("plain bytes"), b"plain NEW\n");
    assert_eq!(file_mode(&plain), 0o600, "private mode survives the update");
    assert_eq!(
        file_mode(&exec),
        0o755,
        "executable bit survives the update"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn patch_create_narrows_mode_by_umask() {
    let (dir, rel) = scratch();
    let extension = extension_with(None);
    let patch_tool = tool_named(&extension, "patch");
    let args = changes(&sonic_rs::json!({ "changes": [
        { "path": format!("{rel}/newborn.txt"), "create": "first breath\n" },
    ] }));
    let text = ok_text(run_approved(&patch_tool, &args).await);
    assert!(text.starts_with("Success"), "create applies, got: {text}");
    let mode = file_mode(&dir.path().join("newborn.txt"));
    assert_eq!(
        mode & 0o022,
        0,
        "new file is 0644 narrowed by umask, never group/other-writable: {mode:o}"
    );
    assert_ne!(mode & 0o400, 0, "owner keeps read: {mode:o}");
}

// ---------------------------------------------------------------------------
// ETP-04/03: exec cwd confinement and argument bounds.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exec_rejects_absolute_escaping_and_missing_cwd() {
    let (dir, rel) = scratch();
    std::fs::create_dir(dir.path().join("work")).expect("cwd fixture");
    let extension = extension_with(None);
    let exec = tool_named(&extension, "exec");

    for bad in [String::from("/tmp"), String::from("..")] {
        let args = changes(&sonic_rs::json!({ "command": "echo hi", "cwd": bad }));
        assert_eq!(
            err_text(run(&exec, &args).await),
            "exec: cwd must be relative to the workspace root",
            "cwd {bad} is refused"
        );
    }
    let args = changes(&sonic_rs::json!({ "command": "echo hi", "cwd": "no-such-dir" }));
    assert_eq!(
        err_text(run(&exec, &args).await),
        "exec: cwd no-such-dir does not exist"
    );

    // Control: a workspace-relative cwd passes validation. The deny-context
    // then refuses at approval, proving validation runs before any spawn.
    let args = changes(&sonic_rs::json!({ "command": "echo hi", "cwd": format!("{rel}/work") }));
    assert!(
        matches!(run(&exec, &args).await, ToolOutcome::Err(_)),
        "inside cwd validates; approval denies in the test context"
    );
}

#[tokio::test]
async fn exec_argument_bounds_reject_bad_values() {
    let extension = extension_with(None);
    let exec = tool_named(&extension, "exec");
    let cases = [
        (
            sonic_rs::json!({ "command": "echo hi", "timeout_seconds": 0 }),
            "exec: timeout_seconds must be between 1 and 86400",
        ),
        (
            sonic_rs::json!({ "command": "echo hi", "timeout_seconds": 86401 }),
            "exec: timeout_seconds must be between 1 and 86400",
        ),
        (
            sonic_rs::json!({ "command": "echo hi", "foreground_s": 0 }),
            "exec: foreground_s must be between 1 and 86400",
        ),
        (
            sonic_rs::json!({ "command": "echo hi", "bogus": 1 }),
            "exec: unknown argument \"bogus\"",
        ),
        (
            sonic_rs::json!({ "command": "   " }),
            "exec: command must not be empty",
        ),
        (
            sonic_rs::json!({ "command": 7 }),
            "exec: command must be a string",
        ),
    ];
    for (value, expected) in cases {
        let args = changes(&value);
        assert_eq!(err_text(run(&exec, &args).await), expected, "args {args}");
    }
}

// ---------------------------------------------------------------------------
// ETP-02: read-only classification through the tool seam.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn exec_classify_separates_read_only_from_mutating() {
    let extension = extension_with(None);
    let exec = tool_named(&extension, "exec");
    let probe = ToolCx::for_test(svc());
    let workspace = probe.workspace().clone();
    for command in ["ls -la", "cat foo.txt", "git status"] {
        let raw = RawValue::parse(&changes(&sonic_rs::json!({ "command": command })))
            .expect("classify args parse");
        match exec.classify(&raw, &workspace) {
            Ok(ToolClass::Exec {
                read_only: true, ..
            }) => {}
            other => panic!("{command} must classify read-only, got: {other:?}"),
        }
    }
    for command in ["rm foo", "echo a && echo b", "export X=1"] {
        let raw = RawValue::parse(&changes(&sonic_rs::json!({ "command": command })))
            .expect("classify args parse");
        match exec.classify(&raw, &workspace) {
            Ok(ToolClass::Exec {
                read_only: false, ..
            }) => {}
            other => panic!("{command} must classify mutating, got: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// RST-08/09: the shared index reflects patch writes (no stale snapshots).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_index_reflects_patch_without_stale_hits() {
    let index_dir = tempfile::tempdir().expect("index root");
    let (dir, rel) = scratch();
    let path = format!("{rel}/idxw.txt");
    std::fs::write(dir.path().join("idxw.txt"), "plain filler\n").expect("seed");

    // One extension owns both tools, so they share the trigram index the way
    // the product wires them.
    let extension = extension_with(Some(index_dir.path().to_path_buf()));
    let search = tool_named(&extension, "search");
    let patch_tool = tool_named(&extension, "patch");

    // Prime the index: the novel token is absent.
    let args = changes(&sonic_rs::json!({
        "mode": "grep",
        "pattern": "wombat_novel_probe",
        "literal": true,
        "path": rel,
    }));
    let primed = ok_text(run(&search, &args).await);
    assert!(
        primed.contains("No matches for wombat_novel_probe"),
        "prime query misses, got: {primed}"
    );
    assert!(
        !primed.contains("[index: full scan"),
        "prime query is index-backed, got: {primed}"
    );

    // Introduce the token through the patch tool, which must dirty the index.
    let args = changes(&sonic_rs::json!({ "changes": [
        { "path": path, "old": "plain filler\n", "new": "wombat_novel_probe appears\n" },
    ] }));
    let text = ok_text(run_approved(&patch_tool, &args).await);
    assert!(
        text.starts_with("Success"),
        "index test patch applies, got: {text}"
    );

    // A stale snapshot would still report no matches; freshness rebuilds.
    let args = changes(&sonic_rs::json!({
        "mode": "grep",
        "pattern": "wombat_novel_probe",
        "literal": true,
        "path": rel,
    }));
    let fresh = ok_text(run(&search, &args).await);
    assert!(
        fresh.contains("idxw.txt:1:"),
        "patched token is searchable, got: {fresh}"
    );
    assert!(
        !fresh.contains("[index: full scan"),
        "fresh query is index-backed, got: {fresh}"
    );
}

#[tokio::test]
async fn search_find_reflects_patch_created_files() {
    let index_dir = tempfile::tempdir().expect("index root");
    let (dir, rel) = scratch();
    std::fs::write(dir.path().join("seed.txt"), "seed\n").expect("seed");

    let extension = extension_with(Some(index_dir.path().to_path_buf()));
    let search = tool_named(&extension, "search");
    let patch_tool = tool_named(&extension, "patch");

    let args = changes(&sonic_rs::json!({
        "mode": "find",
        "pattern": "*.probefind",
        "path": rel,
    }));
    assert!(
        ok_text(run(&search, &args).await).contains("No files match *.probefind"),
        "prime find misses"
    );

    let args = changes(&sonic_rs::json!({ "changes": [
        { "path": format!("{rel}/made.probefind"), "create": "made\n" },
    ] }));
    let text = ok_text(run_approved(&patch_tool, &args).await);
    assert!(text.starts_with("Success"), "create applies, got: {text}");

    let args = changes(&sonic_rs::json!({
        "mode": "find",
        "pattern": "*.probefind",
        "path": rel,
    }));
    assert!(
        ok_text(run(&search, &args).await).contains("made.probefind"),
        "created file is findable without a stale snapshot"
    );
}
