//! Commands, replies, and rejections at the session-control boundary.

use serde::{Deserialize, Serialize, de};

use std::path::PathBuf;

use crate::approval::DenyReason;
use crate::config::{ApprovalMode, Mode};
use crate::content::Part;
use crate::id::{EntryId, JobId, TurnId};
use crate::model::{ModelRoute, ThinkingLevel};
use crate::raw::Tagged;

mod cmd;
mod fields;
mod lex;
mod policy;
mod replies;

pub use cmd::Command;
pub use lex::{Classify, LexError, classify, tokens};
pub use policy::{
    BusyState, CancelScope, Chooser, CommandError, Completion, ErrorTriple, Expect, ExportFormat,
    FrontAction, ImportFailure, Output, Save,
};
pub use replies::{Rejection, Reply};

#[cfg(test)]
mod tests;
