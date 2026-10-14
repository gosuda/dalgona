//! Session tree navigation commands: pickers over live history.
//!
//! Picks resolve client-side into `MoveLeaf`, `Fork`, and `Clone` effects;
//! these handlers only open the picker or start the tracked job.

use dal_agent::ext::command::CommandCx;
use dal_core::command::{Chooser, Command, ErrorTriple, Reply};

/// Opens the session tree picker.
///
/// # Errors
///
/// Returns the empty-session pair when there is nothing to move to.
pub(super) fn tree(cx: &CommandCx<'_>) -> Result<Reply, ErrorTriple> {
    if cx.leaf_entries().is_empty() {
        return Err(empty_session());
    }
    Ok(Reply::Choose {
        chooser: Chooser::Tree,
        filter: "".into(),
    })
}

/// The empty-session pair for `/tree` with nothing to move to.
pub(super) fn empty_session() -> ErrorTriple {
    super::error_triple(
        "The session is empty",
        "there is nothing to move to",
        "Send a message first.",
    )
}

/// Opens the fork-point picker over user messages.
///
/// # Errors
///
/// Returns the no-message pair when the leaf path holds no user message.
pub(super) fn fork(cx: &CommandCx<'_>) -> Result<Reply, ErrorTriple> {
    let has_user = cx
        .leaf_entries()
        .iter()
        .any(|entry| matches!(entry.kind, dal_core::EntryKind::User { .. }));
    if !has_user {
        return Err(no_fork_message());
    }
    Ok(Reply::Choose {
        chooser: Chooser::ForkPoint,
        filter: "".into(),
    })
}

/// The no-message pair for `/fork` without a user message yet.
pub(super) fn no_fork_message() -> ErrorTriple {
    super::error_triple(
        "There is no message to fork from",
        "the session has no user message yet",
        "Send a message first.",
    )
}

/// Copies the whole leaf path into a new session as one tracked job.
///
/// # Errors
///
/// Returns the empty-session pair when there is nothing to clone.
pub(super) fn clone(cx: &CommandCx<'_>) -> Result<Reply, ErrorTriple> {
    if cx.leaf_entries().is_empty() {
        return Err(nothing_to_clone());
    }
    let job = cx.start_job(Command::Clone);
    Ok(Reply::Started(job))
}

/// The empty-session pair for `/clone` with nothing to copy.
pub(super) fn nothing_to_clone() -> ErrorTriple {
    super::error_triple(
        "There is nothing to clone",
        "the session has no messages",
        "Send a message first.",
    )
}
