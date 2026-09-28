//! Durable journal and content-addressed blob primitives.

mod blob;
mod error;
mod journal;
mod layout;
mod list;
mod lock;
mod shard;
mod sidecar;
mod store;
mod util;

pub use blob::{
    INLINE_LIMIT, MAX_BLOB, decode_base64, named_blobs, put, read, share, spill_record,
};
pub use error::{
    AbortedTurn, BlobError, INTERRUPTED_CALL, JournalError, MISSING_ON_BRANCH, NO_EARLIER_SESSION,
    NOT_RUN_CALL, OpenReport, StoreError, TornTail,
};
pub use journal::Receipt;
pub use layout::Locator;
pub use lock::LockGuard;
pub use sidecar::Sidecar;
pub use store::{AppendOutcome, Journal, Store};
pub use util::{FileMode, normalize_name, workspace_key, write_atomic, write_atomic_new};
