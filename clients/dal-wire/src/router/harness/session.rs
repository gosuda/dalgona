//! Harness session selection: header, previous response id, digest, or new.

use super::super::decode::RouterFail;
use super::{HarnessShared, HttpParts};
use dal_agent::{Agent, Host};
use dal_core::{EntryId, ListQuery, PageReq, SessionId};
use sonic_rs::Value;

/// Selects and binds the harness session: header, previous id, digest, or
/// new. A new session keeps the agent that opened it; a selected existing
/// session is resumed.
pub(super) async fn select_session(
    shared: &HarnessShared,
    canon: &[Value],
    previous: Option<String>,
    http: &HttpParts,
) -> Result<(SessionId, Agent), RouterFail> {
    let existing = if let Some(header) = http.session_header.as_deref() {
        Some(header_session(&shared.host, header)?)
    } else if let Some(previous) = previous {
        Some(previous_session(&shared.host, &previous)?)
    } else {
        let digest = crate::router::digest_history(canon);
        shared
            .digests
            .lock()
            .await
            .get(&digest)
            .and_then(|hit| SessionId::parse(hit).ok())
    };
    if let Some(session) = existing {
        return Ok((session, bind_session(shared, session).await?));
    }
    let agent = shared
        .host
        .open(new_session_ref(shared)?, router_client())
        .await
        .map_err(|error| RouterFail::bad("internal", error.to_string()))?;
    let session = agent.view(PageReq::default())?.session.id;
    Ok((session, agent))
}

/// Opens the header-named session or reports its absence.
pub(super) fn header_session(host: &Host, header: &str) -> Result<SessionId, RouterFail> {
    let id = SessionId::parse(header).map_err(|_| {
        RouterFail::missing(
            "session_not_found",
            format!(r#"session "{header}" was not found"#),
        )
    })?;
    let found = host
        .sessions(ListQuery {
            limit: None,
            cursor: None,
            search: None,
        })
        .is_ok_and(|page| page.items.into_iter().any(|info| info.id == id));
    if found {
        Ok(id)
    } else {
        Err(RouterFail::missing(
            "session_not_found",
            format!(r#"session "{header}" was not found"#),
        ))
    }
}

/// Continues the session named by a `resp_<session>.<turn>` id.
pub(super) fn previous_session(host: &Host, previous: &str) -> Result<SessionId, RouterFail> {
    let rest = previous.strip_prefix("resp_").ok_or_else(|| {
        RouterFail::missing(
            "previous_response_not_found",
            format!(r#"previous response "{previous}" was not found"#),
        )
    })?;
    let (session, _) = rest.split_once('.').ok_or_else(|| {
        RouterFail::missing(
            "previous_response_not_found",
            format!(r#"previous response "{previous}" was not found"#),
        )
    })?;
    header_session(host, session).map_err(|_| {
        RouterFail::missing(
            "previous_response_not_found",
            format!(r#"previous response "{previous}" was not found"#),
        )
    })
}

/// Builds a new-session reference at the serve workspace.
pub(super) fn new_session_ref(shared: &HarnessShared) -> Result<dal_agent::SessionRef, RouterFail> {
    let workspace = dal_core::Workspace::try_from(shared.options.workspace.clone())
        .map_err(|error| RouterFail::bad("internal", error.to_string()))?;
    Ok(dal_agent::SessionRef::New {
        workspace,
        name: None,
    })
}

/// Returns the router client id for harness sessions.
pub(super) fn router_client() -> dal_core::ClientId {
    crate::protocol::mint_client_id("router")
}

/// Resumes one existing session for the harness turn.
async fn bind_session(shared: &HarnessShared, session: SessionId) -> Result<Agent, RouterFail> {
    let workspace = find_session_workspace(&shared.host, session)?;
    shared
        .host
        .open(
            dal_agent::SessionRef::Resume {
                key: session.to_string().into(),
                workspace,
            },
            router_client(),
        )
        .await
        .map_err(|error| RouterFail::bad("internal", error.to_string()))
}

/// Finds one session's workspace through the host listing.
pub(super) fn find_session_workspace(
    host: &Host,
    session: SessionId,
) -> Result<dal_core::Workspace, RouterFail> {
    host.sessions(ListQuery {
        limit: None,
        cursor: None,
        search: None,
    })
    .map_err(|error| RouterFail::bad("internal", error.to_string()))?
    .items
    .into_iter()
    .find(|info| info.id == session)
    .map(|info| info.workspace)
    .ok_or_else(|| {
        RouterFail::missing(
            "session_not_found",
            format!(r#"session "{session}" was not found"#),
        )
    })
}

/// Returns true when the session has no entries yet.
pub(super) fn is_fresh_session(head: &dal_core::View) -> bool {
    head.entries.items.is_empty()
}

/// Rejects a harness request while any turn runs.
pub(super) fn check_idle_turn(head: &dal_core::View) -> Result<(), RouterFail> {
    match &head.turn {
        dal_core::TurnState::Idle => Ok(()),
        dal_core::TurnState::Running { turn } | dal_core::TurnState::Settling { turn } => {
            Err(RouterFail::busy(format!(
                "session {} is running turn {turn}: wait for it to end",
                head.session.id
            )))
        }
        dal_core::TurnState::Compacting { job } => Err(RouterFail::busy(format!(
            "session {} is running turn {job}: wait for it to end",
            head.session.id
        ))),
    }
}

/// Returns the active leaf entry before the turn.
pub(super) fn active_leaf(head: &dal_core::View) -> Option<EntryId> {
    head.tree.branches.first().map(|branch| branch.leaf)
}
