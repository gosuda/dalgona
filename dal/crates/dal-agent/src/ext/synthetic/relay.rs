//! Services for a synthetic run outside a session (the router relay):
//! inference works, every other service is unavailable.

use dal_core::Answer;
use dal_core::ext::{McpDeclaration, McpRequest, McpResponse};
use dal_core::{
    AgentsOp, AgentsReply, DenyReason, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
    JobsReply, ModelRequest, Notice, Question, RunOutput, RunRequest, SidecarOp, TurnOp,
    TurnOpReply, Workspace,
};
use dal_provider::EventStream;

use crate::error::ServiceError;
use crate::ext::Caller;
use crate::ext::services::{ServiceFuture, Services};
use crate::ext::tool::{RawValue, Tool, ToolOutcome};
use crate::host::HostShared;
use crate::session::turn::{RequestDeps, infer_stream};
use dal_core::Visibility;
use std::sync::Arc;

pub(super) fn workspace(shared: &HostShared) -> Result<Workspace, dal_core::WorkspaceError> {
    Workspace::new(shared.env.cwd.clone())
}

pub(super) struct RelayServices {
    deps: RequestDeps,
    cancel: tokio_util::sync::CancellationToken,
}

impl RelayServices {
    pub(super) fn new(deps: RequestDeps) -> Self {
        Self {
            deps,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }
}

fn unavailable<T: Send + 'static>() -> ServiceFuture<'static, T> {
    Box::pin(async {
        Err(ServiceError::Denied(DenyReason::Unavailable {
            what: "services outside a session".into(),
        }))
    })
}

fn no_members<T: Send + 'static>() -> ServiceFuture<'static, T> {
    Box::pin(async {
        Err(ServiceError::Denied(DenyReason::Unavailable {
            what: "member sessions outside a session".into(),
        }))
    })
}

impl Services for RelayServices {
    fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }

    fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
        unavailable()
    }

    fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
        unavailable()
    }

    fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
        unavailable()
    }

    fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
        unavailable()
    }

    fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
        unavailable()
    }

    fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
        unavailable()
    }

    fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
        unavailable()
    }

    fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
        no_members()
    }

    fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
        unavailable()
    }

    fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
        unavailable()
    }

    fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<crate::ext::Doc>> {
        unavailable()
    }

    fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
        unavailable()
    }

    fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
        unavailable()
    }

    fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
        Box::pin(async {
            Err(ServiceError::Denied(DenyReason::Unavailable {
                what: "session blobs outside a session".into(),
            }))
        })
    }

    fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async {
            Err(ServiceError::Denied(DenyReason::Unavailable {
                what: "session blobs outside a session".into(),
            }))
        })
    }

    fn infer(&self, _who: &Caller, req: ModelRequest) -> ServiceFuture<'_, Inference> {
        Box::pin(async move {
            let stream = infer_stream(&self.deps, req, &self.cancel).await;
            super::collect(stream)
                .await
                .map_err(|failure| ServiceError::failed(None, failure.to_string()))
        })
    }

    fn infer_stream(&self, _who: &Caller, req: ModelRequest) -> ServiceFuture<'_, EventStream> {
        Box::pin(async move { Ok(infer_stream(&self.deps, req, &self.cancel).await) })
    }

    fn call_tool(
        &self,
        _who: &Caller,
        _name: &str,
        _args: Box<RawValue>,
    ) -> ServiceFuture<'_, ToolOutcome> {
        unavailable()
    }

    fn add_session_tools(
        &self,
        _who: &Caller,
        _tools: Vec<(Arc<dyn Tool>, Visibility)>,
    ) -> ServiceFuture<'_, ()> {
        unavailable()
    }

    fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
        unavailable()
    }

    fn notify(&self, _who: &Caller, _notice: Notice) {}

    fn append_record(
        &self,
        _who: &Caller,
        _kind: &str,
        _body: Box<RawValue>,
    ) -> ServiceFuture<'_, EntryId> {
        unavailable()
    }

    fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
