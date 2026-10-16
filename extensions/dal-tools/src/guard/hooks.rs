//! Guard observer hooks: session and turn lifecycle plus tool-call tracking.

use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use dal_agent::error::ServiceError;
use dal_agent::ext::{BoxFuture, Hook, HookCx, HookError, ObserveHook};
use dal_core::ext::{
    BeforeTurn, SessionEnd, SessionStart, ToolCallEvent, ToolResultEvent, TurnEnd,
};
use dal_core::{CallId, Name, Notice, SessionId, ToolCallVerdict, ToolClass, TurnId, TurnOp};

use super::{
    CallNote, Engine, FileFindings, G8Rule, GuardFindings, Session, TurnState, args_text,
    cut_preview, report, strike, strike_key, summary, warnings,
};
pub(super) struct SessionStartHook(pub(super) Arc<Engine>);
pub(super) struct SessionEndHook(pub(super) Arc<Engine>);
pub(super) struct BeforeTurnHook(pub(super) Arc<Engine>);
pub(super) struct ToolCallHook(pub(super) Arc<Engine>);
pub(super) struct ToolResultHook(pub(super) Arc<Engine>);
pub(super) struct TurnEndHook(pub(super) Arc<Engine>);

impl ObserveHook<SessionStart> for SessionStartHook {
    fn call(&self, input: SessionStart, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let engine = Arc::clone(&self.0);
        Box::pin(async move {
            if !engine.cfg.enabled {
                return Ok(());
            }
            let Ok(mut state) = engine.state.lock() else {
                return Ok(());
            };
            state
                .sessions
                .insert(cx.session, Session::new(input.resumed));
            Ok(())
        })
    }
}

impl ObserveHook<SessionEnd> for SessionEndHook {
    fn call(&self, input: SessionEnd, _cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let engine = Arc::clone(&self.0);
        Box::pin(async move {
            let Ok(mut state) = engine.state.lock() else {
                return Ok(());
            };
            state.sessions.remove(&input.session);
            Ok(())
        })
    }
}

static REDUCTION_ASK: std::sync::LazyLock<Result<regex::Regex, regex::Error>> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(fix|refactor|cleanup|simplify|remove|delete|deduplicate|deslop)\b",
        )
    });

impl Hook<BeforeTurn, Option<String>> for BeforeTurnHook {
    fn call(
        &self,
        input: BeforeTurn,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        let engine = Arc::clone(&self.0);
        Box::pin(async move {
            if !engine.cfg.enabled {
                return Ok(None);
            }
            let reduction_ask = REDUCTION_ASK
                .as_ref()
                .as_ref()
                .is_ok_and(|pattern| pattern.is_match(&input.text));
            let Ok(mut guard) = engine.state.lock() else {
                return Ok(None);
            };
            let state = &mut *guard;
            let Some(session) = state.sessions.get_mut(&cx.session) else {
                return Ok(None);
            };
            state.last_epoch = state.last_epoch.saturating_add(1);
            let pending = session.pending.take().map(|report| report.to_string());
            session.turn = Some(TurnState::new(input.turn, state.last_epoch, reduction_ask));
            Ok(pending)
        })
    }
}

/// What the repeat guard decided for one tool call, settled under the lock.
enum Gate {
    /// The call may run.
    Pass,
    /// The turn is exhausted: block the call, the turn is already stopped.
    Blocked(Box<str>),
    /// This call exhausted the turn: block it and ask the host to cancel.
    Exhausted(Box<str>),
}

fn is_mutating(class: &ToolClass) -> bool {
    matches!(
        class,
        ToolClass::Patch
            | ToolClass::Exec {
                read_only: false,
                ..
            }
            | ToolClass::Eval { pure: false, .. }
    )
}

/// The block reason of an exhausted turn, rendered once at the stop so every
/// later blocked call of the turn carries the same text.
fn exhaustion_reason(turn: &mut TurnState, tool: &Name, key: &[u8; 16]) -> Box<str> {
    if let Some(reason) = &turn.exhaustion {
        return reason.clone();
    }
    let files = turn
        .files
        .iter()
        .map(|path| &**path)
        .collect::<Vec<_>>()
        .join(", ");
    let paths = if files.is_empty() {
        "none"
    } else {
        files.as_str()
    };
    let cause = turn.last_error.as_deref().unwrap_or("none");
    let reason: Box<str> =
        report::exhaustion(&strike::display_key(tool.as_str(), key), cause, paths).into();
    turn.exhaustion = Some(reason.clone());
    reason
}

fn gate_call(engine: &Engine, session: &SessionId, input: ToolCallEvent) -> Gate {
    if !engine.cfg.enabled {
        return Gate::Pass;
    }
    let Ok(mut state) = engine.state.lock() else {
        return Gate::Pass;
    };
    let Some(turn) = state
        .sessions
        .get_mut(session)
        .and_then(|session| session.turn.as_mut())
        .filter(|turn| turn.id == input.turn)
    else {
        return Gate::Pass;
    };
    turn.sequence = turn.sequence.saturating_add(1);
    let sequence = turn.sequence;
    let key = is_mutating(&input.class).then(|| strike_key(&input.tool, &input.args));
    if let Some(key) = key {
        let first_stop = !turn.strikes.stopped();
        if turn.strikes.on_call(key, sequence) == strike::Step::Stop {
            let reason = exhaustion_reason(turn, &input.tool, &key);
            return if first_stop {
                Gate::Exhausted(reason)
            } else {
                Gate::Blocked(reason)
            };
        }
    }
    let note = CallNote {
        command: args_text(&input.args, "command"),
        path: args_text(&input.args, "path"),
        tool: input.tool,
        key,
    };
    turn.calls.insert(input.call, (note, sequence));
    Gate::Pass
}

fn notice(turn: TurnId, kind: &str, text: &str) -> Notice {
    Notice {
        turn: Some(turn),
        kind: kind.into(),
        text: text.into(),
    }
}

/// Reports whether the host withheld authority, by denial or by the user's
/// decline of the grant question.
fn withheld(error: &ServiceError) -> bool {
    matches!(error, ServiceError::Denied(_) | ServiceError::Declined)
}

/// Handles a failed cancel request. When the host withholds the `turn`
/// service the guard keeps blocking and says so once per session; any other
/// failure stays silent because nothing was withheld.
fn note_cancel_failure(engine: &Engine, cx: &HookCx, turn: TurnId, error: &ServiceError) {
    if !withheld(error) {
        return;
    }
    let first_denial = engine
        .state
        .lock()
        .ok()
        .and_then(|mut state| {
            let session = state.sessions.get_mut(&cx.session)?;
            Some(!std::mem::replace(&mut session.turn_denied_noted, true))
        })
        .unwrap_or(false);
    if first_denial {
        cx.services.notify(
            &cx.caller,
            notice(turn, "guard_turn_denied", report::TURN_DENIED),
        );
    }
}

impl Hook<ToolCallEvent, ToolCallVerdict> for ToolCallHook {
    fn call(
        &self,
        input: ToolCallEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<ToolCallVerdict, HookError>> {
        let engine = Arc::clone(&self.0);
        Box::pin(async move {
            let turn = input.turn;
            match gate_call(&engine, &cx.session, input) {
                Gate::Pass => Ok(ToolCallVerdict::Allow),
                Gate::Blocked(reason) => Ok(ToolCallVerdict::Block { reason }),
                Gate::Exhausted(reason) => {
                    if let Err(error) = cx.services.turn(&cx.caller, TurnOp::Cancel).await {
                        note_cancel_failure(&engine, &cx, turn, &error);
                    }
                    Ok(ToolCallVerdict::Block { reason })
                }
            }
        })
    }
}

/// Reports whether `path` is the spill log of the exec call `call`, which the
/// exec tool writes to `<session>/jobs/<call id>.log`. The router reads no
/// other path, so a `Full output:` line forged in command output cannot make
/// the guard read a file on the attacker's behalf.
fn is_call_log(path: &str, call: &CallId) -> bool {
    let path = Path::new(path);
    let named_for_call = path
        .file_name()
        .is_some_and(|name| name == format!("{}.log", call.as_str()).as_str());
    let in_jobs = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|directory| directory == "jobs");
    named_for_call && in_jobs
}

/// One exec result whose full output sits in a spill file the router reads
/// after the lock is released. The epoch ties the read to its turn instance.
struct SpillRead {
    path: Box<str>,
    preview: Box<str>,
    turn: TurnId,
    epoch: u64,
}

/// Folds one tool result into the turn: strike evidence, failure notices,
/// and the warning scan of a result whose text is already in hand. Returns
/// the spill read still owed, when the router may read the full output.
fn record_result(
    engine: &Engine,
    session_id: &SessionId,
    input: ToolResultEvent,
) -> Option<SpillRead> {
    if !engine.cfg.enabled {
        return None;
    }
    let mut state = engine.state.lock().ok()?;
    let session = state.sessions.get_mut(session_id)?;
    let spill_readable = session.fs_read;
    let turn = session.turn.as_mut().filter(|turn| turn.id == input.turn)?;
    let (note, sequence) = turn.calls.get(&input.call)?;
    let (tool, key, command, path, sequence) = (
        note.tool.clone(),
        note.key,
        note.command.clone(),
        note.path.clone(),
        *sequence,
    );
    if !input.ok {
        turn.last_error = Some(cut_preview(&input.preview).into());
    }
    if let Some(key) = key {
        turn.strikes.on_result(key, sequence, input.ok);
        if !input.ok && (1..=2).contains(&turn.strikes.strikes()) {
            let cause = turn.last_error.clone().unwrap_or_default();
            let evidence = path.unwrap_or_else(|| tool.as_str().into());
            turn.pending_notices.push(report::strike_notice(
                turn.strikes.strikes(),
                &format!("{} returned an error", tool.as_str()),
                &cause,
                &evidence,
            ));
        }
    }
    if tool.as_str() != "exec" || command.as_deref().is_some_and(warnings::grep_like) {
        return None;
    }
    match warnings::full_output_path(&input.preview) {
        Some(path) if spill_readable && is_call_log(path, &input.call) => Some(SpillRead {
            path: path.into(),
            turn: input.turn,
            epoch: turn.epoch,
            preview: input.preview,
        }),
        _ => {
            turn.warnings.extend(warnings::scan(&input.preview));
            None
        }
    }
}

/// Commits the warnings of one spill read, or of the preview when the read
/// found nothing. A turn that ended or was replaced during the read keeps
/// its state. A withheld `fs.read` is a fact about the session, so it ends
/// spill reads and is reported once even when the turn it arrived for is gone.
fn commit_spill(
    engine: &Engine,
    cx: &HookCx,
    read: &SpillRead,
    result: &Result<Option<Vec<u8>>, ServiceError>,
) {
    let text = match result {
        Ok(Some(bytes)) => String::from_utf8_lossy(bytes),
        _ => Cow::Borrowed(&*read.preview),
    };
    let found = warnings::scan(&text);
    let denied = result.as_ref().err().is_some_and(withheld);
    let first_denial = {
        let Ok(mut state) = engine.state.lock() else {
            return;
        };
        let Some(session) = state.sessions.get_mut(&cx.session) else {
            return;
        };
        let first_denial = denied && std::mem::take(&mut session.fs_read);
        if let Some(turn) = session
            .turn
            .as_mut()
            .filter(|turn| turn.epoch == read.epoch)
        {
            turn.warnings.extend(found);
        }
        first_denial
    };
    if first_denial {
        cx.services.notify(
            &cx.caller,
            notice(read.turn, "guard_fs_read_denied", report::FS_READ_DENIED),
        );
    }
}

impl ObserveHook<ToolResultEvent> for ToolResultHook {
    fn call(
        &self,
        input: ToolResultEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), HookError>> {
        let engine = Arc::clone(&self.0);
        Box::pin(async move {
            let Some(read) = record_result(&engine, &cx.session, input) else {
                return Ok(());
            };
            let result = cx.services.fs_read(&cx.caller, &read.path).await;
            commit_spill(&engine, &cx, &read, &result);
            Ok(())
        })
    }
}

impl ObserveHook<TurnEnd> for TurnEndHook {
    fn call(&self, input: TurnEnd, cx: HookCx) -> BoxFuture<'static, Result<(), HookError>> {
        let engine = Arc::clone(&self.0);
        Box::pin(async move {
            if !engine.cfg.enabled {
                return Ok(());
            }
            let Ok(mut state) = engine.state.lock() else {
                return Ok(());
            };
            let Some(session) = state.sessions.get_mut(&cx.session) else {
                return Ok(());
            };
            if session
                .turn
                .as_ref()
                .is_none_or(|turn| turn.id != input.turn)
            {
                return Ok(());
            }
            let report_text = {
                let Some(turn) = session.turn.as_mut().filter(|turn| turn.id == input.turn) else {
                    return Ok(());
                };
                summary::build(
                    &engine.cfg,
                    &mut session.dues,
                    &mut session.seen_warnings,
                    turn,
                )
                .map(|text| Arc::from(text.as_str()))
            };
            let Some(turn) = session.turn.as_ref().filter(|turn| turn.id == input.turn) else {
                return Ok(());
            };
            let files: Vec<FileFindings> = {
                let mut files: Vec<_> = turn.findings.values().cloned().collect();
                files.sort_by(|a, b| a.path.cmp(&b.path));
                files
            };
            let warnings: Vec<(Box<str>, u32, Box<str>)> = turn
                .warnings
                .iter()
                .map(|warning| (warning.path.clone(), warning.line, warning.text.clone()))
                .collect();
            let mut stream: Vec<(G8Rule, u32)> = turn
                .stream_counts
                .iter()
                .map(|(rule, count)| (*rule, *count))
                .collect();
            stream.sort();
            let findings = GuardFindings {
                turn: turn.id,
                files,
                warnings,
                stream,
                strikes: turn.strikes.strikes(),
                report: report_text.clone(),
            };
            session.last = Some(Arc::new(findings));
            if session.pending.is_none() {
                session.pending = report_text;
            }
            Ok(())
        })
    }
}
