//! Session-local publication and replay.

pub(crate) mod actor;
pub(crate) mod backend;
pub(crate) mod commands;
pub(crate) mod context;
pub(crate) mod control;
pub(crate) mod dispatch;
pub(crate) mod driver;
pub(crate) mod projection;
pub(crate) mod ring;
pub(crate) mod rt;
pub(crate) mod script;
#[cfg(test)]
mod service_path_checks;
pub(crate) mod shared;
pub(crate) mod status;
pub(crate) mod subscriber;
pub(crate) mod tasks;
pub(crate) mod turn;
use dal_core::ext::{Mail as ExtMail, Receipt};
use dal_core::{
    Answer, BlobId, ClientId, Command, EntryId, Name, Reply, RequestId, SessionId, TurnOp,
    TurnOpReply,
};
use tokio::sync::{mpsc, oneshot};

use self::subscriber::SubscriberShared;
use crate::error::AgentError;

/// Bounded command channel into one session actor.
pub(crate) const COMMAND_CHANNEL: usize = 64;

/// One client operation routed to the session actor.
pub(crate) enum ActorRequest {
    /// Submit a command through the fold.
    Submit {
        /// The submitted command.
        command: Command,
        /// The submitting client.
        by: ClientId,
        /// The fold-produced reply.
        reply: oneshot::Sender<Result<Reply, AgentError>>,
    },
    /// Answer one open request.
    Answer {
        /// The request to resolve.
        id: RequestId,
        /// The client's answer.
        answer: Answer,
        /// The answering client.
        by: ClientId,
        /// Resolution acknowledgement.
        reply: oneshot::Sender<Result<(), AgentError>>,
    },
    /// Read one session blob.
    Blob {
        /// The blob digest.
        id: BlobId,
        /// The blob bytes.
        reply: oneshot::Sender<Result<Vec<u8>, AgentError>>,
    },
    /// Publish one blob through the session journal.
    BlobPut {
        /// Bytes to publish under their BLAKE3 digest.
        bytes: Vec<u8>,
        /// The durable digest or store failure.
        reply: oneshot::Sender<Result<BlobId, AgentError>>,
    },
    /// Poll every status kind now and report the resulting statuses.
    PollStatus {
        /// The busy or texted statuses after the poll.
        reply: oneshot::Sender<Vec<dal_core::ExtStatus>>,
    },
    /// Flush the journal, close subscriber queues, and stop the actor.
    Shutdown {
        /// Shutdown acknowledgement.
        reply: oneshot::Sender<()>,
    },
    /// Report driver work through the fold.
    Work {
        /// The reported work.
        work: actor::TurnWork,
    },
    /// Read or write one sidecar value.
    Sidecar {
        /// The sidecar operation.
        op: SidecarOp,
    },
    /// Run one compare-and-swap state operation (R08).
    State {
        /// The state request.
        req: StateReq,
    },
    /// Deliver one mailbox message to this session.
    Mail {
        /// The mail request.
        req: MailRequest,
    },
    /// Run one turn operation.
    Turn {
        /// The turn request.
        req: TurnRequest,
    },
    /// Fork or clone this session into a new live session.
    Branch {
        /// The fork anchor, or none to clone the active path.
        at: Option<dal_core::EntryId>,
        /// The client the new session binds to.
        by: ClientId,
        /// The new session identity.
        reply: oneshot::Sender<Result<SessionId, AgentError>>,
    },
    /// Journal one attributed synthetic inner inference.
    Inferred {
        /// Who ran the inner call.
        who: dal_core::Owner,
        /// Why it ran.
        purpose: dal_core::InferredPurpose,
        /// Its normalized usage.
        usage: dal_core::Usage,
    },
    /// Journal one extension record on the current leaf.
    ExtRecord {
        /// The append request.
        req: ExtRecordRequest,
    },
    /// Attach the capability-scoped services for hook dispatch.
    Services {
        /// The session services.
        services: std::sync::Arc<dyn crate::ext::Services>,
    },
}

/// One actor-owned state operation with its reply channel (R08).
pub(crate) struct StateReq {
    /// The compare-and-swap operation.
    pub op: dal_core::ext::StateOp,
    /// The resulting record, or the expected failure.
    pub reply: oneshot::Sender<Result<dal_core::ext::StateRecord, dal_core::ext::StateError>>,
}

/// One actor-owned sidecar read or write.
pub(crate) enum SidecarOp {
    /// Read one sidecar value.
    Read {
        /// The sidecar name.
        name: Name,
        /// The stored bytes, absent when never written.
        reply: oneshot::Sender<Option<Vec<u8>>>,
    },
    /// Write one sidecar value.
    Write {
        /// The sidecar name.
        name: Name,
        /// The bytes to store.
        bytes: Vec<u8>,
        /// Write acknowledgement.
        reply: oneshot::Sender<()>,
    },
}

/// One durable extension-record append with its reply channel.
pub(crate) struct ExtRecordRequest {
    /// The owning extension.
    pub ext: Name,
    /// The extension-defined record kind.
    pub kind: Box<str>,
    /// The opaque record body.
    pub body: dal_core::RawJson,
    /// The journal position after the receipt, or the append failure.
    pub reply: oneshot::Sender<Result<dal_core::EntryId, crate::error::ServiceError>>,
}

/// One mailbox delivery to this session.
pub(crate) enum MailRequest {
    /// Store one message and report its receipt.
    Send {
        /// The message to store.
        mail: ExtMail,
        /// The delivery receipt.
        reply: oneshot::Sender<Option<Receipt>>,
    },
    /// Reads the non-destructive mailbox page after one cursor.
    Recv {
        /// The last mailbox cursor already read.
        after: Option<EntryId>,
        /// The messages and next cursor, oldest first.
        reply: oneshot::Sender<(Vec<ExtMail>, Option<EntryId>)>,
    },
}

/// One turn operation with its reply channel.
pub(crate) struct TurnRequest {
    /// The turn operation.
    pub op: TurnOp,
    /// The operation reply.
    pub reply: oneshot::Sender<TurnOpReply>,
}

/// The client end of a live subscription queue.
pub(crate) type SubscriberPort = std::sync::Arc<SubscriberShared>;

/// The actor port one `Agent` holds; dropping it never stops the actor.
#[derive(Clone)]
pub(crate) struct SessionHandle {
    session: SessionId,
    tx: mpsc::Sender<ActorRequest>,
}

impl SessionHandle {
    /// A port into the actor behind the bounded command channel.
    pub(crate) fn new(session: SessionId, tx: mpsc::Sender<ActorRequest>) -> Self {
        Self { session, tx }
    }

    /// The session this port addresses.
    pub(crate) fn session(&self) -> SessionId {
        self.session
    }

    async fn roundtrip<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<T>) -> ActorRequest,
    ) -> Result<T, AgentError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(build(tx))
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })?;
        rx.await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Submits a command; full channels apply backpressure to the caller.
    pub(crate) async fn submit(&self, command: Command, by: ClientId) -> Result<Reply, AgentError> {
        self.roundtrip(|reply| ActorRequest::Submit { command, by, reply })
            .await?
    }

    /// Answers one open request through the broker.
    pub(crate) async fn answer(
        &self,
        id: RequestId,
        answer: Answer,
        by: ClientId,
    ) -> Result<(), AgentError> {
        self.roundtrip(|reply| ActorRequest::Answer {
            id,
            answer,
            by,
            reply,
        })
        .await?
    }

    /// Reads one session blob through the journal.
    pub(crate) async fn blob(&self, id: BlobId) -> Result<Vec<u8>, AgentError> {
        self.roundtrip(|reply| ActorRequest::Blob { id, reply })
            .await?
    }
    /// Publishes one content-addressed blob through the journal.
    pub(crate) async fn put_blob(&self, bytes: Vec<u8>) -> Result<BlobId, AgentError> {
        self.roundtrip(|reply| ActorRequest::BlobPut { bytes, reply })
            .await?
    }

    /// Polls every status kind through the actor and returns the statuses
    /// that are busy or carry text.
    pub(crate) async fn poll_status(&self) -> Result<Vec<dal_core::ExtStatus>, AgentError> {
        self.roundtrip(|reply| ActorRequest::PollStatus { reply })
            .await
    }

    /// Flushes the journal, closes subscriber queues, and stops the actor.
    pub(crate) async fn shutdown(&self) {
        let _ = self
            .roundtrip(|reply| ActorRequest::Shutdown { reply })
            .await;
    }

    /// Journals one synthetic inner inference; a closed session drops it.
    pub(crate) async fn inferred(
        &self,
        who: dal_core::Owner,
        purpose: dal_core::InferredPurpose,
        usage: dal_core::Usage,
    ) {
        let _ = self
            .tx
            .send(ActorRequest::Inferred {
                who,
                purpose,
                usage,
            })
            .await;
    }

    /// Reports driver work through the fold; full channels apply
    /// backpressure to the caller.
    pub(crate) async fn work(&self, work: actor::TurnWork) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Work { work })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Reads or writes one sidecar value.
    pub(crate) async fn sidecar(&self, op: SidecarOp) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Sidecar { op })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Runs one compare-and-swap state operation (R08).
    pub(crate) async fn state(&self, req: StateReq) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::State { req })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Delivers one mailbox message.
    pub(crate) async fn mail(&self, req: MailRequest) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Mail { req })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }
    /// Attaches the capability-scoped services for hook dispatch.
    pub(crate) async fn services(
        &self,
        services: std::sync::Arc<dyn crate::ext::Services>,
    ) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Services { services })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Journals one extension record.
    pub(crate) async fn ext_record(&self, req: ExtRecordRequest) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::ExtRecord { req })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Runs one turn operation.
    pub(crate) async fn turn(&self, req: TurnRequest) -> Result<(), AgentError> {
        self.tx
            .send(ActorRequest::Turn { req })
            .await
            .map_err(|_| AgentError::SessionClosed { id: self.session })
    }

    /// Forks or clones this session, returning the new session identity.
    pub(crate) async fn branch(
        &self,
        at: Option<dal_core::EntryId>,
        by: ClientId,
    ) -> Result<SessionId, AgentError> {
        self.roundtrip(|reply| ActorRequest::Branch { at, by, reply })
            .await?
    }
}
