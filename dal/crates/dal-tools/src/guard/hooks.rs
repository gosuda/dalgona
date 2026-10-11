//! Guard observer hooks: session and turn lifecycle plus tool-call tracking.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use dal_agent::ext::{BoxFuture, Hook, HookCx, HookError, ObserveHook};
use dal_core::ToolClass;
use dal_core::ext::{
    BeforeTurn, SessionEnd, SessionStart, ToolCallEvent, ToolResultEvent, TurnEnd,
};

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
            state.sessions.insert(
                cx.session,
                Session {
                    reset_due: input.resumed,
                    seen_warnings: HashSet::new(),
                    turn: None,
                    pending: None,
                    announced_blocks: false,
                    last: None,
                },
            );
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
            state.turns.retain(|_, session| *session != input.session);
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
            let Ok(mut state) = engine.state.lock() else {
                return Ok(None);
            };
            let Some(session) = state.sessions.get_mut(&cx.session) else {
                return Ok(None);
            };
            let pending = session.pending.take().map(|report| report.to_string());
            session.turn = Some(TurnState {
                id: input.turn,
                reduction_ask,
                counted: HashSet::new(),
                added: 0,
                deleted: 0,
                files: BTreeSet::new(),
                new_files: BTreeSet::new(),
                deletions: BTreeMap::new(),
                churn: BTreeMap::new(),
                strikes: strike::Stream::default(),
                last_error: None,
                pending_notices: Vec::new(),
                sequence: 0,
                calls: HashMap::new(),
                first_pre: BTreeMap::new(),
                last_post: BTreeMap::new(),
                bands: Vec::new(),
                warnings: Vec::new(),
                stream_counts: BTreeMap::new(),
                fired: BTreeSet::new(),
                findings: BTreeMap::new(),
                notices: HashSet::new(),
            });
            state.turns.insert(input.turn, cx.session);
            Ok(pending)
        })
    }
}

impl Hook<ToolCallEvent, dal_core::ToolCallVerdict> for ToolCallHook {
    fn call(
        &self,
        input: ToolCallEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<dal_core::ToolCallVerdict, HookError>> {
        let engine = Arc::clone(&self.0);
        Box::pin(async move {
            if !engine.cfg.enabled {
                return Ok(dal_core::ToolCallVerdict::Allow);
            }
            let Ok(mut state) = engine.state.lock() else {
                return Ok(dal_core::ToolCallVerdict::Allow);
            };
            let Some(session) = state.sessions.get_mut(&cx.session) else {
                return Ok(dal_core::ToolCallVerdict::Allow);
            };
            let Some(turn) = session.turn.as_mut().filter(|turn| turn.id == input.turn) else {
                return Ok(dal_core::ToolCallVerdict::Allow);
            };
            let mutating = matches!(
                input.class,
                ToolClass::Patch
                    | ToolClass::Exec {
                        read_only: false,
                        ..
                    }
                    | ToolClass::Eval { pure: false, .. }
            );
            let key = mutating.then(|| strike_key(&input.tool, &input.args));
            let command = args_text(&input.args, "command");
            let path = args_text(&input.args, "path");
            turn.sequence = turn.sequence.saturating_add(1);
            let sequence = turn.sequence;
            if let Some(key) = key {
                turn.strikes.on_call(key, sequence);
            }
            turn.calls.insert(
                input.call,
                (
                    CallNote {
                        tool: input.tool,
                        key,
                        command,
                        path,
                    },
                    sequence,
                ),
            );
            Ok(dal_core::ToolCallVerdict::Allow)
        })
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
            if !engine.cfg.enabled {
                return Ok(());
            }
            let Ok(mut state) = engine.state.lock() else {
                return Ok(());
            };
            let Some(session) = state.sessions.get_mut(&cx.session) else {
                return Ok(());
            };
            let Some(turn) = session.turn.as_mut().filter(|turn| turn.id == input.turn) else {
                return Ok(());
            };
            let (tool, key, command, path, sequence) = match turn.calls.get(&input.call) {
                Some((note, sequence)) => (
                    note.tool.clone(),
                    note.key,
                    note.command.clone(),
                    note.path.clone(),
                    *sequence,
                ),
                None => return Ok(()),
            };
            if !input.ok {
                turn.last_error = Some(cut_preview(&input.preview).into());
            }
            if let Some(key) = key {
                turn.strikes.on_result(key, sequence, input.ok);
                if !input.ok && (1..=2).contains(&turn.strikes.strikes()) {
                    let cause = turn.last_error.clone().unwrap_or_default();
                    let evidence = path.clone().unwrap_or_else(|| tool.as_str().into());
                    turn.pending_notices.push(report::strike_notice(
                        turn.strikes.strikes(),
                        &format!("{} returned an error", tool.as_str()),
                        &cause,
                        &evidence,
                    ));
                }
            }
            if tool.as_str() == "exec"
                && command
                    .as_deref()
                    .is_none_or(|command| !warnings::grep_like(command))
            {
                let text = warnings::full_output_path(&input.preview)
                    .map_or_else(|| input.preview.to_string(), str::to_owned);
                turn.warnings.extend(warnings::scan(&text));
            }
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
                    &mut session.reset_due,
                    &mut session.announced_blocks,
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
