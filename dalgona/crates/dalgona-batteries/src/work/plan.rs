// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::HashMap;
use std::sync::Mutex;

use dal_agent::error::ServiceError;
use dal_agent::ext::{Caller, Services, ToolOutcome};
use dal_core::approval::ToolClass;
use dal_core::ext::ToolCallVerdict;
use dal_core::{Answer, Choice, Preview, Question, SessionId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(crate) const PLAN_KIND: &str = "plan";
pub(crate) const PLAN_PROMPT: &str = "Plan mode is on. Explore with read-only tools; submit a plan with the plan tool for approval. Prefer evidence over questions; ask only when a choice is irreversible, destructive, safety-critical, or cross-cutting.";
pub(crate) const SELECT_TITLE: &str = "Plan ready for review";
const APPROVE: &str = "Approve";
const REVISE: &str = "Revise";
const REJECT: &str = "Reject";
pub(crate) const OPTIONS: [&str; 3] = [APPROVE, REVISE, REJECT];
const REVISE_PROMPT: &str = "How should the plan change?";
const REJECT_PROMPT: &str = "Why was the plan rejected?";
const PLAN_ON: &str = "Plan mode is on.";
const PLAN_OFF: &str = "Plan mode is off.";
const PLAN_USAGE: &str = "usage: /plan [on|off]";
const APPROVED: &str = "Plan approved. Plan mode is off.";
const EXPIRED: &str = "Plan approval request expired.";
pub(crate) const MAX_PLAN_BYTES: usize = 16_384;
pub(crate) const MAX_SUMMARY_BYTES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Off,
    Planning,
    Awaiting,
}

impl Phase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Planning => "planning",
            Self::Awaiting => "awaiting",
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct BatteryState {
    phases: Mutex<HashMap<SessionId, Phase>>,
}

impl BatteryState {
    pub(crate) fn phase(&self, session: SessionId) -> Phase {
        self.lock_phases()
            .get(&session)
            .copied()
            .unwrap_or(Phase::Off)
    }

    pub(crate) fn set_phase(&self, session: SessionId, phase: Phase) {
        self.lock_phases().insert(session, phase);
    }

    pub(crate) fn command(&self, session: SessionId, args: &str) -> CommandAction {
        let mut phases = self.lock_phases();
        let phase = phases.get(&session).copied().unwrap_or(Phase::Off);
        let action = command_action(phase, args);
        match action {
            CommandAction::Set(next) => {
                phases.insert(session, next);
            }
            CommandAction::CancelAwaiting => {
                phases.insert(session, Phase::Off);
            }
            CommandAction::AlreadyAwaiting | CommandAction::Usage => {}
        }
        action
    }

    pub(crate) fn claim(&self, session: SessionId) -> Result<(), super::Error> {
        let mut phases = self.lock_phases();
        let phase = phases.get(&session).copied().unwrap_or(Phase::Off);
        submission_allowed(phase)?;
        phases.insert(session, Phase::Awaiting);
        Ok(())
    }

    pub(crate) fn settle(&self, session: SessionId, next: Phase) {
        let mut phases = self.lock_phases();
        if phases.get(&session) == Some(&Phase::Awaiting) {
            phases.insert(session, next);
        }
    }

    pub(crate) fn session_start(&self, session: SessionId) {
        self.set_phase(session, Phase::Off);
    }

    pub(crate) fn session_end(&self, session: SessionId) {
        self.lock_phases().remove(&session);
    }

    fn lock_phases(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Phase>> {
        self.phases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Arguments for the `plan` tool: the Markdown body under review plus a short summary.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanArgs {
    /// The complete Markdown plan submitted for approval.
    pub plan: String,
    /// A short human-readable summary of the plan.
    pub summary: String,
}

#[derive(Serialize)]
pub(crate) struct PlanRecord<'a> {
    pub summary: &'a str,
    pub state: &'a str,
    pub mode_after: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandAction {
    Set(Phase),
    CancelAwaiting,
    AlreadyAwaiting,
    Usage,
}

pub(crate) fn command_action(phase: Phase, args: &str) -> CommandAction {
    match args.trim() {
        "" => match phase {
            Phase::Off => CommandAction::Set(Phase::Planning),
            Phase::Planning => CommandAction::Set(Phase::Off),
            Phase::Awaiting => CommandAction::AlreadyAwaiting,
        },
        "on" => match phase {
            Phase::Off | Phase::Planning => CommandAction::Set(Phase::Planning),
            Phase::Awaiting => CommandAction::AlreadyAwaiting,
        },
        "off" => match phase {
            Phase::Awaiting => CommandAction::CancelAwaiting,
            Phase::Off | Phase::Planning => CommandAction::Set(Phase::Off),
        },
        _ => CommandAction::Usage,
    }
}

pub(crate) struct CommandEffect {
    pub text: &'static str,
    pub cancel_turn: bool,
}

pub(crate) fn command(
    state: &BatteryState,
    session: SessionId,
    args: &str,
) -> Result<CommandEffect, super::Error> {
    let (text, cancel_turn) = match state.command(session, args) {
        CommandAction::Set(Phase::Planning) => (PLAN_ON, false),
        CommandAction::Set(Phase::Off | Phase::Awaiting) => (PLAN_OFF, false),
        CommandAction::CancelAwaiting => (PLAN_OFF, true),
        CommandAction::AlreadyAwaiting => return Err(super::Error::AlreadyAwaiting),
        CommandAction::Usage => (PLAN_USAGE, false),
    };
    Ok(CommandEffect { text, cancel_turn })
}

pub(crate) fn submission_allowed(phase: Phase) -> Result<(), super::Error> {
    match phase {
        Phase::Planning => Ok(()),
        Phase::Off => Err(super::Error::NotInPlanMode),
        Phase::Awaiting => Err(super::Error::AlreadyAwaiting),
    }
}

pub(crate) fn validate_plan(plan: &str, summary: &str) -> Result<(), super::Error> {
    if plan.is_empty() || plan.len() > MAX_PLAN_BYTES {
        return Err(super::Error::PlanTooLarge { bytes: plan.len() });
    }
    if summary.is_empty() || summary.len() > MAX_SUMMARY_BYTES {
        return Err(super::Error::SummaryTooLarge {
            bytes: summary.len(),
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) struct Host<'a> {
    pub services: &'a dyn Services,
    pub caller: &'a Caller,
    pub session: SessionId,
}

impl Host<'_> {
    async fn record(
        &self,
        summary: &str,
        outcome: &str,
        mode_after: bool,
    ) -> Result<(), ServiceError> {
        let record = PlanRecord {
            summary,
            state: outcome,
            mode_after,
        };
        super::append(self.services, self.caller, PLAN_KIND, &record).await
    }

    async fn ask(&self, question: Question) -> Result<Option<String>, ServiceError> {
        let answer = self.services.ask(self.caller, question).await?;
        Ok(answer.and_then(answer_text))
    }

    async fn feedback(&self, prompt: &str) -> Result<Option<String>, ServiceError> {
        let question = Question::Text {
            prompt: prompt.into(),
            placeholder: None,
        };
        let text = self.ask(question).await?;
        Ok(text
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty()))
    }
}

fn answer_text(answer: Answer) -> Option<String> {
    let Answer::Value(value) = answer else {
        return None;
    };
    sonic_rs::from_str::<String>(value.as_str()).ok()
}

fn select_question(args: &PlanArgs) -> Question {
    Question::Select {
        prompt: SELECT_TITLE.into(),
        options: OPTIONS
            .iter()
            .map(|label| Choice {
                label: (*label).into(),
                description: None,
            })
            .collect(),
        multi: false,
        preview: Some(Preview {
            title: args.summary.as_str().into(),
            body: args.plan.as_str().into(),
            digest: None,
        }),
    }
}

pub(crate) async fn submit(
    state: &BatteryState,
    args: &str,
    host: Host<'_>,
) -> Result<String, ToolOutcome> {
    let args = sonic_rs::from_str::<PlanArgs>(args)
        .map_err(|error| super::failure(format!("plan: invalid input: {error}")))?;
    submission_allowed(state.phase(host.session)).map_err(super::failure)?;
    validate_plan(&args.plan, &args.summary).map_err(super::failure)?;
    state.claim(host.session).map_err(super::failure)?;
    let reply = review(state, host, &args).await;
    state.settle(host.session, Phase::Planning);
    reply.map_err(super::service_outcome)
}

async fn review(
    state: &BatteryState,
    host: Host<'_>,
    args: &PlanArgs,
) -> Result<String, ServiceError> {
    host.record(&args.summary, "submitted", true).await?;
    let selected = host.ask(select_question(args)).await?;
    match selected.as_deref() {
        Some(APPROVE) => {
            conclude(state, host, &args.summary, "approved", Phase::Off).await?;
            Ok(APPROVED.to_owned())
        }
        Some(REVISE) => {
            let feedback = host.feedback(REVISE_PROMPT).await?;
            conclude(state, host, &args.summary, "revised", Phase::Planning).await?;
            Ok(match feedback {
                Some(feedback) => format!("Plan returned for revision: {feedback}"),
                None => "Plan returned for revision.".to_owned(),
            })
        }
        Some(REJECT) => {
            let feedback = host.feedback(REJECT_PROMPT).await?;
            conclude(state, host, &args.summary, "rejected", Phase::Off).await?;
            Ok(match feedback {
                Some(feedback) => format!("Plan rejected. {feedback}"),
                None => "Plan rejected.".to_owned(),
            })
        }
        _ => {
            conclude(state, host, &args.summary, "expired", Phase::Off).await?;
            Ok(EXPIRED.to_owned())
        }
    }
}

async fn conclude(
    state: &BatteryState,
    host: Host<'_>,
    summary: &str,
    outcome: &str,
    next: Phase,
) -> Result<(), ServiceError> {
    let appended = host.record(summary, outcome, next == Phase::Planning).await;
    match &appended {
        Ok(()) => state.settle(host.session, next),
        Err(_) => state.set_phase(host.session, Phase::Off),
    }
    appended
}

pub(crate) fn before_turn(phase: Phase, items: &[super::todo::TodoItem]) -> Option<String> {
    let todos = super::todo::render(items);
    match (phase, todos.is_empty()) {
        (Phase::Off, true) => None,
        (Phase::Off, false) => Some(todos),
        (Phase::Planning | Phase::Awaiting, true) => Some(PLAN_PROMPT.to_owned()),
        (Phase::Planning | Phase::Awaiting, false) => Some(format!("{PLAN_PROMPT}\n\n{todos}")),
    }
}

pub(crate) fn verdict(phase: Phase, class: &ToolClass, tool: &str) -> ToolCallVerdict {
    if phase == Phase::Off {
        return ToolCallVerdict::Allow;
    }
    if matches!(class, ToolClass::Read | ToolClass::Eval { pure: true }) {
        return ToolCallVerdict::Allow;
    }
    ToolCallVerdict::Block {
        reason: format!(
            "plan mode is on: \"{tool}\" may change the workspace; submit a plan with the plan tool, or ask the user to run /plan off"
        )
        .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked_reason(verdict: ToolCallVerdict) -> Option<String> {
        match verdict {
            ToolCallVerdict::Allow => None,
            ToolCallVerdict::Block { reason } => Some(reason.into()),
            ToolCallVerdict::Rewrite { .. } => Some("unexpected rewrite".to_owned()),
            _ => Some("unexpected verdict".to_owned()),
        }
    }

    #[test]
    fn plan_verdict_allows_reads_and_pure_eval() {
        assert_eq!(
            verdict(Phase::Planning, &ToolClass::Read, "read"),
            ToolCallVerdict::Allow
        );
        assert_eq!(
            verdict(Phase::Awaiting, &ToolClass::Eval { pure: true }, "eval"),
            ToolCallVerdict::Allow
        );
    }

    #[test]
    fn plan_verdict_blocks_every_unproved_mutating_class() {
        for class in [
            ToolClass::Patch,
            ToolClass::Exec {
                read_only: true,
                grant: None,
            },
            ToolClass::Eval { pure: false },
            ToolClass::Other,
        ] {
            assert_eq!(
                blocked_reason(verdict(Phase::Planning, &class, "patch")),
                Some("plan mode is on: \"patch\" may change the workspace; submit a plan with the plan tool, or ask the user to run /plan off".to_owned())
            );
        }
    }

    #[test]
    fn plan_verdict_allows_every_class_when_off() {
        assert_eq!(
            verdict(Phase::Off, &ToolClass::Other, "unknown"),
            ToolCallVerdict::Allow
        );
    }

    #[test]
    fn plan_command_trims_and_toggles_arguments() {
        assert!(matches!(
            command_action(Phase::Off, "  "),
            CommandAction::Set(Phase::Planning)
        ));
        assert!(matches!(
            command_action(Phase::Planning, "  "),
            CommandAction::Set(Phase::Off)
        ));
        assert!(matches!(
            command_action(Phase::Off, " on "),
            CommandAction::Set(Phase::Planning)
        ));
        assert!(matches!(
            command_action(Phase::Planning, " off "),
            CommandAction::Set(Phase::Off)
        ));
    }

    #[test]
    fn plan_command_cancels_only_explicit_off_while_awaiting() {
        assert!(matches!(
            command_action(Phase::Awaiting, "off"),
            CommandAction::CancelAwaiting
        ));
        assert!(matches!(
            command_action(Phase::Awaiting, "on"),
            CommandAction::AlreadyAwaiting
        ));
        assert!(matches!(
            command_action(Phase::Awaiting, ""),
            CommandAction::AlreadyAwaiting
        ));
    }

    #[test]
    fn plan_command_rejects_other_arguments() {
        assert!(matches!(
            command_action(Phase::Off, "enable"),
            CommandAction::Usage
        ));
    }

    #[test]
    fn plan_prompt_is_added_at_turn_boundary() {
        assert_eq!(before_turn(Phase::Off, &[]), None);
        assert_eq!(
            before_turn(Phase::Planning, &[]),
            Some(PLAN_PROMPT.to_owned())
        );
        assert_eq!(
            before_turn(
                Phase::Off,
                &[super::super::todo::TodoItem {
                    subject: "task".to_owned(),
                    description: String::new(),
                    state: super::super::todo::TodoState::Pending,
                }]
            ),
            Some("- [ ] task".to_owned())
        );
    }

    #[test]
    fn plan_submission_checks_mode_before_submission() {
        assert_eq!(
            submission_allowed(Phase::Off).map_err(|error| error.to_string()),
            Err("the plan tool runs only while plan mode is on".to_owned())
        );
        assert_eq!(
            submission_allowed(Phase::Awaiting).map_err(|error| error.to_string()),
            Err("a plan is already awaiting review".to_owned())
        );
        assert!(submission_allowed(Phase::Planning).is_ok());
    }

    #[test]
    fn plan_size_validation_uses_utf8_byte_lengths() {
        assert_eq!(
            validate_plan("", "ok").map_err(|error| error.to_string()),
            Err("plan is 0 bytes; the cap is 16384".to_owned())
        );
        assert_eq!(
            validate_plan("x".repeat(MAX_PLAN_BYTES + 1).as_str(), "ok")
                .map_err(|error| error.to_string()),
            Err("plan is 16385 bytes; the cap is 16384".to_owned())
        );
        assert_eq!(
            validate_plan("ok", "é".repeat(101).as_str()).map_err(|error| error.to_string()),
            Err("summary is 202 bytes; the cap is 200".to_owned())
        );
        assert!(validate_plan("ok", "é".repeat(100).as_str()).is_ok());
    }

    #[test]
    fn plan_state_resets_on_open_and_is_removed_on_close() {
        let state = BatteryState::default();
        let session = SessionId::new_v7();
        state.set_phase(session, Phase::Planning);
        state.session_start(session);
        assert_eq!(state.phase(session), Phase::Off);
        state.set_phase(session, Phase::Awaiting);
        state.session_end(session);
        assert_eq!(state.phase(session), Phase::Off);
    }
}

#[cfg(test)]
mod host_tests {
    use dal_core::approval::ToolClass;
    use dal_core::{Preview, Question};

    use super::super::support::{Scripted, ScriptedWorkHost, TestResult};
    use super::{OPTIONS, PLAN_PROMPT, Phase, SELECT_TITLE};

    fn plan_args(plan: &str, summary: &str) -> Result<String, sonic_rs::Error> {
        Ok(format!(
            r#"{{"plan":{},"summary":{}}}"#,
            sonic_rs::to_string(plan)?,
            sonic_rs::to_string(summary)?
        ))
    }

    fn blocked(tool: &str) -> Option<String> {
        Some(format!(
            "plan mode is on: \"{tool}\" may change the workspace; submit a plan with the plan tool, or ask the user to run /plan off"
        ))
    }

    #[derive(serde::Deserialize)]
    struct StateRecord {
        state: String,
        mode_after: bool,
    }

    fn states(host: &ScriptedWorkHost) -> Result<Vec<(String, bool)>, sonic_rs::Error> {
        host.services
            .all_bodies("plan")
            .iter()
            .map(|body| {
                sonic_rs::from_str::<StateRecord>(body)
                    .map(|record| (record.state, record.mode_after))
            })
            .collect()
    }

    #[tokio::test]
    async fn plan_mode_blocks_before_approval() -> TestResult {
        let host = ScriptedWorkHost::open();
        assert_eq!(host.plan_command("on"), "Plan mode is on.");

        let exec = ToolClass::Exec {
            read_only: true,
            grant: None,
        };
        let refused = [
            ("patch", ToolClass::Patch),
            ("exec", exec),
            ("eval", ToolClass::Eval { pure: false }),
            ("custom", ToolClass::Other),
        ];
        for (tool, class) in refused {
            assert_eq!(host.guard(tool, class).await?, blocked(tool));
        }
        let allowed = [
            ("read", ToolClass::Read),
            ("web_fetch", ToolClass::Read),
            ("todo", ToolClass::Read),
            ("plan", ToolClass::Read),
            ("eval", ToolClass::Eval { pure: true }),
        ];
        for (tool, class) in allowed {
            assert_eq!(host.guard(tool, class).await?, None);
        }
        assert_eq!(host.services.asked_count(), 0);
        assert_eq!(host.services.side_calls(), 0);

        assert_eq!(host.plan_command("off"), "Plan mode is off.");
        assert_eq!(host.guard("patch", ToolClass::Patch).await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn plan_mode_prompt() -> TestResult {
        let host = ScriptedWorkHost::open();
        assert_eq!(host.before_turn().await?, None);

        host.plan_command("on");
        assert_eq!(host.before_turn().await?, Some(PLAN_PROMPT.to_owned()));

        host.todo_tool(r#"{"action":"write","todos":[{"subject":"a","state":"pending"}]}"#)
            .await;
        assert_eq!(
            host.before_turn().await?,
            Some(format!("{PLAN_PROMPT}\n\n- [ ] a"))
        );

        host.plan_command("off");
        assert_eq!(host.before_turn().await?, Some("- [ ] a".to_owned()));
        Ok(())
    }

    #[tokio::test]
    async fn plan_approve() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.plan_command("on");
        host.services.script([Scripted::Label("Approve")]);

        let result = host
            .plan(&plan_args("# Plan\n\nDo it", "Ship it")?)
            .await;

        assert_eq!(result, "Plan approved. Plan mode is off.");
        let Some(Question::Select {
            prompt,
            options,
            multi,
            preview,
        }) = host.services.question(0)
        else {
            return Err("the plan review is not a selection".into());
        };
        assert_eq!(&*prompt, SELECT_TITLE);
        let labels: Vec<&str> = options.iter().map(|option| &*option.label).collect();
        assert_eq!(labels, OPTIONS);
        assert!(options.iter().all(|option| option.description.is_none()));
        assert!(!multi);
        assert_eq!(
            preview,
            Some(Preview {
                title: "Ship it".into(),
                body: "# Plan\n\nDo it".into(),
                digest: None,
            })
        );
        assert_eq!(
            host.services.all_bodies("plan"),
            [
                r#"{"summary":"Ship it","state":"submitted","mode_after":true}"#,
                r#"{"summary":"Ship it","state":"approved","mode_after":false}"#,
            ]
        );
        assert_eq!(host.state.phase(host.session), Phase::Off);
        let (quiet, _) = host.status()?;
        assert!(quiet);
        assert_eq!(host.guard("patch", ToolClass::Patch).await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn plan_revise() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.plan_command("on");
        host.services
            .script([Scripted::Label("Revise"), Scripted::Label("rename the tables")]);

        let result = host.plan(&plan_args("# Plan", "Ship it")?).await;

        assert_eq!(result, "Plan returned for revision: rename the tables");
        assert_eq!(
            host.services.question(1),
            Some(Question::Text {
                prompt: "How should the plan change?".into(),
                placeholder: None,
            })
        );
        assert_eq!(
            host.services.all_bodies("plan"),
            [
                r#"{"summary":"Ship it","state":"submitted","mode_after":true}"#,
                r#"{"summary":"Ship it","state":"revised","mode_after":true}"#,
            ]
        );
        assert_eq!(host.state.phase(host.session), Phase::Planning);
        assert_eq!(host.guard("patch", ToolClass::Patch).await?, blocked("patch"));
        Ok(())
    }

    #[tokio::test]
    async fn plan_reject() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.plan_command("on");
        host.services
            .script([Scripted::Label("Reject"), Scripted::Label("too risky")]);
        let with_feedback = host.plan(&plan_args("# Plan", "Ship it")?).await;
        assert_eq!(with_feedback, "Plan rejected. too risky");
        assert_eq!(
            host.services.question(1),
            Some(Question::Text {
                prompt: "Why was the plan rejected?".into(),
                placeholder: None,
            })
        );
        assert_eq!(host.state.phase(host.session), Phase::Off);

        host.plan_command("on");
        host.services
            .script([Scripted::Label("Reject"), Scripted::Dismissed]);
        let dismissed = host.plan(&plan_args("# Plan", "Ship it")?).await;
        assert_eq!(dismissed, "Plan rejected.");
        assert_eq!(host.state.phase(host.session), Phase::Off);
        assert_eq!(
            states(&host)?,
            [
                ("submitted".to_owned(), true),
                ("rejected".to_owned(), false),
                ("submitted".to_owned(), true),
                ("rejected".to_owned(), false),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn plan_tool_outside_mode() -> TestResult {
        let host = ScriptedWorkHost::open();
        let args = plan_args("# Plan", "Ship it")?;
        assert_eq!(
            host.tool("plan", &args).await?,
            "the plan tool runs only while plan mode is on"
        );
        assert_eq!(host.services.asked_count(), 0);
        assert!(host.services.all_bodies("plan").is_empty());

        host.plan_command("on");
        host.services.script([Scripted::Held]);
        let (first, (second, asked)) = tokio::join!(host.plan(&args), async {
            let asked = host.services.wait_asked().await;
            let second = if asked {
                host.plan(&args).await
            } else {
                String::new()
            };
            host.services.cancel_ask();
            (second, asked)
        });

        assert!(asked);
        assert_eq!(second, "a plan is already awaiting review");
        assert_eq!(first, "interrupted");
        assert_eq!(host.services.asked_count(), 1);
        assert_eq!(states(&host)?, [("submitted".to_owned(), true)]);
        Ok(())
    }

    #[tokio::test]
    async fn plan_request_timeout() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.plan_command("on");
        host.services.script([Scripted::Dismissed]);
        let expired = host.plan(&plan_args("# Plan", "Ship it")?).await;
        assert_eq!(expired, "Plan approval request expired.");
        assert_eq!(
            states(&host)?,
            [
                ("submitted".to_owned(), true),
                ("expired".to_owned(), false),
            ]
        );
        assert_eq!(host.state.phase(host.session), Phase::Off);

        let cancelled = ScriptedWorkHost::open();
        cancelled.plan_command("on");
        cancelled.services.script([Scripted::Held]);
        let args = plan_args("# Plan", "Ship it")?;
        let (result, asked) = tokio::join!(cancelled.plan(&args), async {
            let asked = cancelled.services.wait_asked().await;
            cancelled.services.cancel_ask();
            asked
        });
        assert!(asked);
        assert_eq!(result, "interrupted");
        assert_eq!(states(&cancelled)?, [("submitted".to_owned(), true)]);
        assert_eq!(cancelled.state.phase(cancelled.session), Phase::Planning);
        Ok(())
    }

    #[tokio::test]
    async fn plan_size_caps() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.plan_command("on");
        let cases = [
            (plan_args(&"x".repeat(16_385), "s")?, "plan is 16385 bytes; the cap is 16384"),
            (plan_args("", "s")?, "plan is 0 bytes; the cap is 16384"),
            (plan_args("p", &"s".repeat(201))?, "summary is 201 bytes; the cap is 200"),
            (plan_args("p", "")?, "summary is 0 bytes; the cap is 200"),
        ];
        for (args, expected) in cases {
            assert_eq!(host.plan(&args).await, expected);
        }
        assert_eq!(host.services.asked_count(), 0);
        assert!(host.services.all_bodies("plan").is_empty());
        assert_eq!(host.state.phase(host.session), Phase::Planning);

        host.services.script([Scripted::Label("Approve")]);
        let boundary = plan_args(&"x".repeat(16_384), &"s".repeat(200))?;
        assert_eq!(host.plan(&boundary).await, "Plan approved. Plan mode is off.");
        assert_eq!(host.services.asked_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn plan_mode_resets() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.plan_command("on");
        host.services.script([Scripted::Held]);
        let args = plan_args("# Plan", "Ship it")?;

        let (result, (asked, driven)) = tokio::join!(host.plan(&args), async {
            let asked = host.services.wait_asked().await;
            let driven = async {
                if !asked {
                    return Err("plan selection was not opened".into());
                }
                host.close_session().await?;
                host.open_session().await?;
                assert_eq!(host.state.phase(host.session), Phase::Off);
                assert_eq!(host.guard("patch", ToolClass::Patch).await?, None);
                assert_eq!(host.before_turn().await?, None);
                assert_eq!(host.services.asked_count(), 1);
                TestResult::Ok(())
            }
            .await;
            host.services.cancel_ask();
            (asked, driven)
        });
        assert!(asked);
        driven?;

        assert_eq!(result, "interrupted");
        assert_eq!(states(&host)?, [("submitted".to_owned(), true)]);
        assert_eq!(host.state.phase(host.session), Phase::Off);
        Ok(())
    }

    #[tokio::test]
    async fn plan_off_cancels_awaiting_selection() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.plan_command("on");
        host.services.script([Scripted::Held]);
        let args = plan_args("# Plan", "Ship it")?;

        let (result, (asked, driven)) = tokio::join!(host.plan(&args), async {
            let asked = host.services.wait_asked().await;
            let driven = async {
                if !asked {
                    return Err("plan selection was not opened".into());
                }
                assert_eq!(host.plan_command("on"), "a plan is already awaiting review");
                assert_eq!(host.plan_command(""), "a plan is already awaiting review");
                let (quiet, _) = host.status()?;
                assert!(!quiet);
                assert_eq!(host.plan_command("off"), "Plan mode is off.");
                TestResult::Ok(())
            }
            .await;
            host.services.cancel_ask();
            (asked, driven)
        });
        assert!(asked);
        driven?;

        assert_eq!(result, "interrupted");
        assert_eq!(host.state.phase(host.session), Phase::Off);
        assert_eq!(states(&host)?, [("submitted".to_owned(), true)]);
        let (quiet, _) = host.status()?;
        assert!(quiet);
        Ok(())
    }

    #[tokio::test]
    async fn plan_record_failure() -> TestResult {
        let host = ScriptedWorkHost::open();
        host.plan_command("on");
        host.services.fail_appends([true]);
        let args = plan_args("# Plan", "Ship it")?;
        assert_eq!(host.plan(&args).await, "the journal write failed");
        assert_eq!(host.services.asked_count(), 0);
        assert!(host.services.all_bodies("plan").is_empty());
        assert_eq!(host.state.phase(host.session), Phase::Planning);

        host.services.script([Scripted::Label("Approve")]);
        host.services.fail_appends([false, true]);
        assert_eq!(host.plan(&args).await, "the journal write failed");
        assert_eq!(host.services.asked_count(), 1);
        assert_eq!(states(&host)?, [("submitted".to_owned(), true)]);
        assert_eq!(host.state.phase(host.session), Phase::Off);
        Ok(())
    }
}
