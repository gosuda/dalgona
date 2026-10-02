//! Session ownership, durable event processing, and host operations for dal.
//!
//! A single actor owns each live session. It applies the core fold, persists
//! state-changing records before publishing updates, and provides one
//! operation contract to local and remote clients.

mod admission;
mod agent;
mod broker;
pub mod error;
pub mod ext;
mod host;
pub(crate) mod jobs;
pub(crate) mod proc;
mod scheme;
pub(crate) mod session;

pub use agent::{Agent, Delivery, Subscription};
pub use broker::Broker;
pub use error::{AgentError, DenyReason, HostError, ServiceError, ToolError, ValidationError};
pub use ext::grants::{GrantKey, GrantStore, GrantStoreError, PersistentGrant};
pub use host::{
    DocEntry, Env, Host, HostSubscription, HostUpdate, Product, SessionRef, ShutdownReport,
};
pub use proc::sandbox::sandbox_notice;
pub use proc::{
    FULL_OUTPUT_PREFIX, OUTPUT_FILE_CAP_BYTES, PREVIEW_BYTES, PROGRESS_LINES, PROGRESS_PERIOD,
    Proc, ProcResult, ProcStatus, SpawnOpts, StopReason, TRUNCATION_MARKER,
};
