//! Service contract: the [`Services`] trait, the actor data-plane, and
//! construction inputs.
//!
//! This module owns the shapes; `super` owns the [`SessionServices`]
//! implementation and every gate. [`ServiceFuture`] is the future alias the
//! rest of the extension runtime imports.

use dal_core::ext::{McpDeclaration, McpRequest, McpResponse};
use dal_core::{
    AgentsOp, AgentsReply, Answer, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
    JobsReply, ModelRequest, Name, Notice, Question, RunOutput, RunRequest, SidecarOp, Site,
    TurnOp, TurnOpReply, Visibility, Workspace,
};
use dal_provider::EventStream;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::Broker;
use crate::error::ServiceError;
use crate::ext::generation::Generation;
use crate::ext::grants::GrantStore;
use crate::ext::mcp::McpClient;
use crate::ext::overlay::Overlay;
use crate::ext::tool::{RawValue, Tool, ToolCxRuntime, ToolOutcome};
use crate::ext::{Caller, ExtRecord};

/// A service future: the async capability surface shared by every method.
///
/// [`GrantStore::ensure`] returns this shape; this module owns the alias the
/// rest of the extension runtime imports.
pub type ServiceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ServiceError>> + Send + 'a>>;

/// Service access for hook and handler contexts.
///
/// The trait spells every script and Rust service exactly as planned: the
/// elided future binds `&self`, so implementations move owned copies into
/// the future instead of holding caller borrows across an await.
pub trait Services: Send + Sync + 'static {
    /// Reads one file through the host file service.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, or backend failure.
    fn fs_read(&self, who: &Caller, path: &str) -> ServiceFuture<'_, Option<Vec<u8>>>;
    /// Writes one file through the host file service.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, or backend failure.
    fn fs_write(&self, who: &Caller, path: &str, bytes: Vec<u8>) -> ServiceFuture<'_, ()>;
    /// Fetches one HTTP request through the host network service.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, or backend failure.
    fn net(&self, who: &Caller, req: FetchRequest) -> ServiceFuture<'_, FetchResponse>;
    /// Runs one process through the approval ladder and checked launcher.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, ladder, or spawn failure.
    fn run(&self, who: &Caller, req: RunRequest) -> ServiceFuture<'_, RunOutput>;
    /// Reads one environment key; there is no enumeration operation.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, or backend failure.
    fn env(&self, who: &Caller, key: &str) -> ServiceFuture<'_, Option<String>>;
    /// Asks the answering client one question; grant-free by design.
    ///
    /// # Errors
    ///
    /// Returns the inject failure, cancellation, or headless-busy failure.
    fn ask(&self, who: &Caller, question: Question) -> ServiceFuture<'_, Option<Answer>>;
    /// Calls one MCP tool; fails closed without a registered client.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, availability, or client failure.
    fn mcp(&self, who: &Caller, req: McpRequest) -> ServiceFuture<'_, McpResponse>;
    /// Lists every MCP block a loaded skill declares in the current
    /// generation, in extension then skill order.
    ///
    /// Reading declarations needs no inject and no grant; the `mcp` grant
    /// gates the calls the declarations lead to, per caller.
    ///
    /// # Errors
    ///
    /// Never fails today; the future shape matches every other service.
    fn mcp_declarations(&self, who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>>;
    /// Registers tools for the current session under the caller's name,
    /// replacing the caller's earlier set; an empty list clears it.
    ///
    /// The tools reach the model only at the next turn boundary, so a turn's
    /// advertised tool bytes never change. A `Deferred` tool is listed as
    /// `Model` from the turn after its first successful call, which the fold
    /// journals as `ToolPromoted`. The set drops at session end and once the
    /// caller's extension leaves the generation. Calls run through the
    /// `tool_call` and `tool_result` hooks and the approval ladder like any
    /// tool.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ToolNameInUse`] when a name repeats, names a
    /// generation tool, or names another caller's session tool; nothing is
    /// registered then.
    fn add_session_tools(
        &self,
        who: &Caller,
        tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()>;
    /// Returns the assistant turn texts the caller's session held when it
    /// opened, oldest first, at most the newest 32.
    ///
    /// Only text blocks of the entries on the journal's root-to-leaf path
    /// count. A fresh session has none; a resumed or branched one has its
    /// prior turns. The list is fixed at session open and needs no inject or
    /// grant.
    ///
    /// # Errors
    ///
    /// Never fails today; the future shape matches every other service.
    fn history_texts(&self, who: &Caller) -> ServiceFuture<'_, Vec<String>>;
    /// Runs one child-agent operation.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, or backend failure.
    fn agents(&self, who: &Caller, op: AgentsOp) -> ServiceFuture<'_, AgentsReply>;
    /// Runs one background-job operation.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, or backend failure.
    fn jobs(&self, who: &Caller, op: JobsOp) -> ServiceFuture<'_, JobsReply>;
    /// Returns the number of unresolved requests in this session's broker.
    ///
    /// This reads the broker directly and never duplicates ask-battery state.
    /// The caller must inject `ask`; no grant is needed.
    fn open_asks(&self, who: &Caller) -> ServiceFuture<'_, usize>;
    /// Resolves one registered scheme URI as a read-only cross-extension
    /// query. An absent scheme or document returns `None`.
    fn scheme(&self, who: &Caller, uri: &str) -> ServiceFuture<'_, Option<crate::ext::Doc>>;
    /// Runs one turn operation against the active turn.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, or backend failure.
    fn turn(&self, who: &Caller, op: TurnOp) -> ServiceFuture<'_, TurnOpReply>;
    /// Reads or writes one extension sidecar value.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, availability, or backend failure.
    fn sidecar(&self, who: &Caller, op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>>;
    /// Runs trusted Rust-only inference: no inject check and no grant.
    ///
    /// # Errors
    ///
    /// Returns the backend failure.
    fn infer(&self, who: &Caller, req: ModelRequest) -> ServiceFuture<'_, Inference>;
    /// Streams trusted Rust-only inference: no inject check and no grant.
    ///
    /// # Errors
    ///
    /// Returns the backend failure.
    fn infer_stream(&self, who: &Caller, req: ModelRequest) -> ServiceFuture<'_, EventStream>;
    /// Calls one registered tool by name. Rust only; never projected into
    /// the Starlark `ctx`.
    ///
    /// # Errors
    ///
    /// Returns the inject, grant, or backend failure.
    fn call_tool(
        &self,
        who: &Caller,
        name: &str,
        args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome>;
    /// Delivers one notice as ordinary session traffic. Fire and forget:
    /// never gated, never fallible.
    fn notify(&self, who: &Caller, notice: Notice);
    /// Journals one caller-owned record on the current leaf and returns its
    /// journal position: the id the next tree entry takes. The record is
    /// durable before the future resolves.
    ///
    /// # Errors
    ///
    /// Returns the journal failure that stopped the append.
    fn append_record(
        &self,
        who: &Caller,
        kind: &str,
        body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId>;
    /// Returns the caller's own records of one kind on the current leaf
    /// path, oldest first. A leaf move changes the answer; the records stay.
    ///
    /// # Errors
    ///
    /// Never fails today; the signature keeps the service future shape.
    fn records(&self, who: &Caller, kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>>;
    /// Publishes bytes under their durable BLAKE3 content digest.
    ///
    /// This Rust-only service is capability-free. The digest is acknowledged
    /// only after the session store has durably published the bytes.
    ///
    /// # Errors
    ///
    /// Returns a typed session-store failure.
    fn blob_put(&self, who: &Caller, bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]>;
    /// Reads one session blob by its BLAKE3 content digest.
    ///
    /// This Rust-only service is capability-free. A missing digest returns
    /// `None`.
    ///
    /// # Errors
    ///
    /// Returns a typed session-store failure.
    fn blob_get(&self, who: &Caller, digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>>;
}

/// Host data-plane behind [`super::SessionServices`], implemented once by the
/// session actor alongside [`ToolCxRuntime`].
///
/// Every method performs its operation with no inject or grant checks; all
/// gates live in [`super::SessionServices`]. Test doubles script these directly.
pub(crate) trait SessionBackend: Send + Sync + 'static {
    /// Reads one file's bytes, or `None` when absent.
    fn fs_read(&self, path: &str) -> ServiceFuture<'_, Option<Vec<u8>>>;
    /// Writes one file's bytes atomically.
    fn fs_write(&self, path: &str, bytes: Vec<u8>) -> ServiceFuture<'_, ()>;
    /// Performs one HTTP exchange.
    fn net(&self, req: FetchRequest) -> ServiceFuture<'_, FetchResponse>;
    /// Runs one child-agent operation.
    fn agents(&self, op: AgentsOp) -> ServiceFuture<'_, AgentsReply>;
    /// Runs one background-job operation for `owner`; only that extension may
    /// settle rows it started.
    fn jobs(&self, owner: &Name, op: JobsOp) -> ServiceFuture<'_, JobsReply>;
    /// Resolves one URI through the registered extension-scheme path.
    fn scheme(&self, caller: &Caller, uri: &str) -> ServiceFuture<'_, Option<crate::ext::Doc>>;
    /// Writes one fixed artifact under this session's job-scoped isolation
    /// directory using atomic mode-0600 publication.
    fn sidecar_artifact(
        &self,
        job: dal_core::JobId,
        file: dal_core::ArtifactFile,
        bytes: Vec<u8>,
    ) -> ServiceFuture<'_, ()>;
    /// Runs one turn operation.
    fn turn(&self, op: TurnOp) -> ServiceFuture<'_, TurnOpReply>;
    /// Journals one extension record on the current leaf and returns its
    /// journal position after the receipt.
    fn append_record(&self, ext: &Name, kind: &str, body: RawValue) -> ServiceFuture<'_, EntryId>;
    /// Returns the extension records on the current leaf path, all
    /// extensions, in journal order. Reads a published snapshot, never the
    /// actor, so a hook that the actor is waiting on can still read.
    fn ext_records(&self) -> Arc<[ExtRecord]>;
    /// Publishes bytes under the session's durable content-addressed key.
    fn blob_put(&self, bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]>;
    /// Reads a session blob by its raw BLAKE3 digest; missing content is `None`.
    fn blob_get(&self, digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>>;
    /// Reads one sidecar value, or `None` when absent.
    fn sidecar_read(&self, name: &Name) -> ServiceFuture<'_, Option<Vec<u8>>>;
    /// Writes one sidecar value atomically.
    fn sidecar_write(&self, name: &Name, bytes: Vec<u8>) -> ServiceFuture<'_, ()>;
    /// Runs one inference to completion.
    fn infer(&self, req: ModelRequest) -> ServiceFuture<'_, Inference>;
    /// Runs one inference with the script host and invocation cancellation.
    fn infer_with_script(
        &self,
        req: ModelRequest,
        _script: Option<Arc<crate::session::script::SessionScriptHost>>,
        _cancel: CancellationToken,
    ) -> ServiceFuture<'_, Inference> {
        self.infer(req)
    }
    /// Opens one inference stream.
    fn infer_stream(&self, req: ModelRequest) -> ServiceFuture<'_, EventStream>;
    /// Calls one registered tool by name.
    fn call_tool(&self, name: &str, args: &RawValue) -> ServiceFuture<'_, ToolOutcome>;
    /// Publishes one session update generated by a host-owned service path.
    fn publish_update(&self, update: dal_core::UpdateKind);
    /// Emits one notice to the session.
    fn notify(&self, notice: Notice);
    /// Reads one environment key; implementations hold a snapshot so
    /// enumeration is impossible by construction.
    fn env(&self, key: &str) -> Option<String>;
}

/// Construction inputs for [`super::SessionServices`], held by the host.
pub(crate) struct SessionServicesDeps {
    /// The shared capability gate; one store serves every service.
    pub(crate) grants: Arc<GrantStore>,
    /// The session request broker for `ask` and run approvals.
    pub(crate) broker: Arc<Broker>,
    /// The actor-owned data-plane.
    pub(crate) backend: Arc<dyn SessionBackend>,
    /// The checked run launcher, shared with [`ToolCxRuntime::spawn`].
    pub(crate) rt: Arc<dyn ToolCxRuntime>,
    /// The registered MCP client, when `dalgona` configured one.
    pub(crate) mcp_client: Option<Arc<dyn McpClient>>,
    /// The host's current extension generation; the declaration feed reads it
    /// so a `/reload` shows in the next listing.
    pub(crate) generation: watch::Receiver<Arc<Generation>>,
    /// The session tool overlay the driver snapshots at turn boundaries.
    pub(crate) overlay: Arc<Overlay>,
    /// The assistant turn texts the session held when it opened.
    pub(crate) history: Arc<[String]>,
    /// The `dal.plugin` declaration span per extension name, when known.
    pub(crate) sites: HashMap<Name, Option<Site>>,
    /// The session cancellation token.
    pub(crate) cancel: CancellationToken,
    /// How long one `ask` question may stay open.
    pub(crate) ask_timeout: Duration,
    /// Whether this session is ephemeral and holds no sidecar directory.
    pub(crate) ephemeral: bool,
    /// The session workspace; the `run` fallback working directory.
    pub(crate) workspace: Workspace,
}
