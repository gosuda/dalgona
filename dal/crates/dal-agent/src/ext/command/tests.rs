//! Reload-prefix test through the live publication path.
//!
//! Publishes a product base with one plugin, reloads with a replacement
//! plugin, and reads the live generation back: the kept prefix is every
//! non-plugin extension, in canonical order, followed by the new plugins.

use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;

use dal_core::{
    AgentsOp, AgentsReply, Answer, ApprovalMode, AutoCompaction, EntryId, EntryView, FetchRequest,
    FetchResponse, Inference, JobsOp, JobsReply, Mode, ModelRequest, ModelRoute, Name, Notice,
    Origin, Page, Question, RunOutput, RunRequest, ServiceSet, SessionId, SessionSummary,
    SidecarOp, ThinkingLevel, TurnOp, TurnOpReply,
};
use dal_core::{SessionInfo, SettingsView, Stats, TreeOutline, TurnState, Usage, UsageView, View};
use tokio::sync::watch;

use super::{CatalogView, CommandCx, CommandHost, ReloadSummary, ResolveMiss, SaveError};
use crate::error::ServiceError;
use crate::ext::generation::{Generation, ValidatedExtensions};
use crate::ext::services::ServiceFuture;
use crate::ext::tool::{RawValue, ToolOutcome};
use crate::ext::{BoxFuture, Caller, CallerKind, Extension, ExtensionBuilder, Services};

struct FakeServices;

impl Services for FakeServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("publish path never calls services")
    }
    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unreachable!("publish path never calls services")
    }
    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unreachable!("publish path never calls services")
    }
    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unreachable!("publish path never calls services")
    }
    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unreachable!("publish path never calls services")
    }
    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unreachable!("publish path never calls services")
    }
    fn mcp(
        &self,
        _who: &Caller,
        _req: dal_core::ext::McpRequest,
    ) -> ServiceFuture<'_, dal_core::ext::McpResponse> {
        unreachable!("publish path never calls services")
    }
    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        unreachable!("publish path never calls services")
    }
    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        unreachable!("publish path never calls services")
    }
    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("publish path never calls services")
    }
    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(
            std::sync::Arc<dyn crate::ext::tool::Tool>,
            dal_core::Visibility,
        )>,
    ) -> ServiceFuture<'_, ()> {
        unreachable!("publish path never calls services")
    }
    fn mcp_declarations(
        &self,
        _who: &Caller,
    ) -> ServiceFuture<'_, Vec<dal_core::ext::McpDeclaration>> {
        unreachable!("publish path never calls services")
    }
    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        unreachable!("publish path never calls services")
    }
    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        unreachable!("publish path never calls services")
    }
    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unreachable!("publish path never calls services")
    }
    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<crate::ext::Doc>> {
        unreachable!("publish path never calls services")
    }
    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unreachable!("publish path never calls services")
    }
    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unreachable!("publish path never calls services")
    }
    fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
        unreachable!("publish path never calls services")
    }
    fn infer_stream(
        &self,
        _who: &Caller,
        _req: ModelRequest,
    ) -> ServiceFuture<'_, dal_provider::EventStream> {
        unreachable!("publish path never calls services")
    }
    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unreachable!("publish path never calls services")
    }
    fn notify(&self, _who: &Caller, _notice: Notice) {}
    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unreachable!("publish path never calls services")
    }
    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        unreachable!("publish path never calls services")
    }
}

struct FakeHost {
    extensions: Vec<Extension>,
    tx: watch::Sender<Arc<Generation>>,
    view: View,
}

impl CommandHost for FakeHost {
    fn view(&self, _caller: &Caller, _session: SessionId, _turn: Option<dal_core::TurnId>) -> View {
        self.view.clone()
    }
    fn data_root(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
    ) -> PathBuf {
        PathBuf::from("/tmp/dal-test")
    }
    fn session_file(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
    ) -> Option<PathBuf> {
        None
    }
    fn log_path(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
    ) -> PathBuf {
        PathBuf::from("/tmp/dal-test/session.log")
    }
    fn submit_wait(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        _command: dal_core::Command,
    ) -> BoxFuture<'_, Result<(), SaveError>> {
        Box::pin(async { unreachable!("publish path never submits") })
    }
    fn start_job(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        _command: dal_core::Command,
    ) -> dal_core::JobId {
        unreachable!("publish path never starts jobs")
    }
    fn cancel_turn(&self, _caller: &Caller, _session: SessionId, _turn: Option<dal_core::TurnId>) {}
    fn sessions_page(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        _limit: u32,
        _cursor: Option<&str>,
        _search: Option<&str>,
    ) -> Page<SessionSummary, Box<str>> {
        unreachable!("publish path never lists sessions")
    }
    fn resolve_session(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        _query: &str,
    ) -> Result<SessionSummary, ResolveMiss> {
        unreachable!("publish path never resolves sessions")
    }
    fn resolve_model(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        _query: &str,
    ) -> Result<ModelRoute, ResolveMiss> {
        unreachable!("publish path never resolves models")
    }
    fn catalog(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
    ) -> Option<CatalogView> {
        None
    }
    fn levels_for(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
    ) -> Vec<ThinkingLevel> {
        Vec::new()
    }
    fn auth_stored(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
    ) -> Vec<(Box<str>, Box<str>)> {
        Vec::new()
    }
    fn auth_remove(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        _provider: &str,
    ) -> bool {
        false
    }
    fn docs_page(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        _uri: &str,
    ) -> Box<str> {
        Box::default()
    }
    fn changelog_uri(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
    ) -> &'static str {
        "dal://changelog"
    }
    fn edit_style_for(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        _route: &ModelRoute,
    ) -> Box<str> {
        Box::default()
    }
    fn leaf_entries(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
    ) -> Vec<EntryView> {
        Vec::new()
    }
    fn history_page(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        _query: dal_core::PageReq,
    ) -> BoxFuture<'_, Result<Page<EntryView>, ServiceError>> {
        Box::pin(async { unreachable!("publish path never reads history") })
    }
    fn current_extensions(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
    ) -> Vec<Extension> {
        self.extensions.clone()
    }
    fn publish_generation(
        &self,
        _caller: &Caller,
        _session: SessionId,
        _turn: Option<dal_core::TurnId>,
        generation: Generation,
    ) {
        self.tx.send_replace(Arc::new(generation));
    }
}

fn ext(name: &str, origin: Origin) -> Extension {
    ExtensionBuilder::new(name, "1.0.0", ServiceSet::EMPTY)
        .expect("valid extension identity")
        .with_origin(origin, None)
        .build()
        .expect("test extension builds")
}

fn test_view(session: SessionId, workspace: dal_core::Workspace) -> View {
    View {
        r#gen: dal_core::Gen::new(NonZeroU64::MIN),
        seq: dal_core::Seq::new(NonZeroU64::MIN),
        session: SessionInfo {
            id: session,
            name: None,
            preview: Box::default(),
            workspace,
            updated_at: dal_core::Timestamp::now(),
            created_at: None,
            archived: None,
            last_seq: None,
        },
        turn: TurnState::Idle,
        entries: Page {
            items: Vec::new(),
            next_before: None,
        },
        tree: TreeOutline {
            branches: Vec::new(),
        },
        settings: SettingsView {
            model: None,
            thinking: ThinkingLevel::Off,
            approval: ApprovalMode::Ask,
            mode: Mode::Normal,
            name: None,
        },
        open: Vec::new(),
        changes: Vec::new(),
        usage: UsageView {
            usage: Usage {
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_write_tokens: 0,
                cost_usd: None,
            },
            context_tokens: 0,
            context_window: 0,
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

#[tokio::test]
async fn reload_prefix_keeps_product_base() {
    let session = SessionId::new_v7();
    let workspace_temp = tempfile::tempdir().expect("workspace tempdir");
    let workspace =
        dal_core::Workspace::new(workspace_temp.path().to_path_buf()).expect("test workspace");
    let first = ValidatedExtensions::validate(
        vec![
            ext("a", Origin::Builtin),
            ext("b", Origin::Builtin),
            ext("battery", Origin::Bundled),
            ext("p1", Origin::User),
        ],
        None,
    )
    .expect("product base validates");
    let live = Arc::new(Generation::build(first));
    let (tx, _) = watch::channel(Arc::clone(&live));
    let caller = Caller::new(
        Name::parse("test").expect("literal name parses"),
        Origin::Builtin,
        ServiceSet::EMPTY,
        CallerKind::Handler,
        None,
    );
    let services: Arc<dyn Services> = Arc::new(FakeServices);
    let host = Arc::new(FakeHost {
        extensions: live.extensions.to_vec(),
        tx: tx.clone(),
        view: test_view(session, workspace),
    });
    let cx = CommandCx::new(caller, session, None, services, host);
    let summary: ReloadSummary = cx
        .publish_plugins(vec![ext("p2", Origin::User)])
        .await
        .expect("reload publishes");
    assert_eq!(summary.plugins, 1);
    let names: Vec<String> = tx
        .borrow()
        .extensions
        .iter()
        .map(|ext| ext.name().to_owned())
        .collect();
    assert_eq!(names, ["a", "b", "battery", "p2"].map(str::to_owned));
}
