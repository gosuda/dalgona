//! Session-owned TTSR setup, prompt rendering, and the `rule://` scheme.

use std::any::Any;
use std::sync::{Arc, Mutex};

use dal_agent::error::SchemeError;
use dal_agent::ext::{
    BoxFuture, Extension, ExtensionBuilder, HookError, PromptOrder, PromptSection, SchemeCx,
    SchemeResolver, SectionCx, SectionFn, SessionCx, SessionInstalls, SessionSetup,
};
use dal_core::{Notice, RegistrationError, ServiceSet};

use super::build::{self, RuleBuildInput, RuleSet, for_agent, set_for_with_judged};
use super::gate::Gate;
use super::judged::{self, JudgeGate};
use super::record::RecordSource;
use super::texts::{RulesProduct, load_problems_note};
use super::watch::factory::{EditStylePicker, TtsrWatchFactory};
/// Product-provided dependencies for the TTSR session lifecycle.
#[derive(Clone)]
pub struct TtsrDeps {
    /// Selects the patch argument dialect for one resolved model route.
    pub pick_edit_style: EditStylePicker,
}

impl std::fmt::Debug for TtsrDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TtsrDeps")
            .field("pick_edit_style", &"<product closure>")
            .finish()
    }
}

/// Immutable TTSR state shared by the watcher, prompt section, and scheme.
#[derive(Debug)]
pub struct TtsrSessionState {
    /// The retained rule snapshot for this session's agent view.
    pub rules: Arc<RuleSet>,
    /// The durable repeat gate, refreshed from visible seed facts per request.
    pub gate: Arc<Mutex<Gate>>,
    /// The strong session judge handle, even when no judged rule is watched.
    pub judge: crate::judge::Judge,
}

#[derive(Clone, Debug)]
struct TtsrSetup {
    deps: TtsrDeps,
}

impl SessionSetup for TtsrSetup {
    fn open(&self, cx: SessionCx) -> BoxFuture<'static, Result<SessionInstalls, HookError>> {
        let deps = self.deps.clone();
        Box::pin(async move { open_session(deps, cx).await })
    }
}

async fn open_session(deps: TtsrDeps, cx: SessionCx) -> Result<SessionInstalls, HookError> {
    let judge_config =
        crate::judge::JudgeConfig::from_config(&cx.config).map_err(|error| HookError::Failed {
            message: format!("dal.toml: {error}").into(),
        })?;
    let judge = crate::judge::Judge::open(crate::judge::JudgeOpen {
        session: cx.session,
        config: judge_config,
        services: Arc::clone(&cx.services),
        caller: cx.caller.clone(),
        session_route: cx.model.clone(),
        session_model_id: cx.model_id.clone(),
    })
    .await
    .map_err(|error| HookError::Failed {
        message: error.to_string().into(),
    })?;

    let complete = rules_for_session(&cx);
    let judge_ready = matches!(judge.state(), crate::judge::Gate::Ready { .. });
    let gate = judged::gate_mode(
        cx.config.rules().judge,
        judge_ready,
        complete.judged_count(),
    )
    .map_err(|error| HookError::Failed {
        message: error.to_string().into(),
    })?;
    let rules = match &gate {
        JudgeGate::Enabled => complete,
        JudgeGate::Disabled { .. } => build::without_judged(&complete),
    };
    let rules = Arc::new(rules);
    let repeat_gate = Arc::new(Mutex::new(Gate::default()));
    let factory = TtsrWatchFactory::new(
        Arc::clone(&rules),
        Arc::clone(&repeat_gate),
        cx.config.rules().clone(),
        cx.workspace.as_path().to_path_buf(),
        deps.pick_edit_style,
        Some(judge.clone()),
    );
    let mut notices = Vec::new();
    let product = if cx.product == "dalgona" {
        RulesProduct::Dalgona
    } else {
        RulesProduct::Dalgon
    };
    if let Some(text) = load_problems_note(product, rules.skipped_count()) {
        notices.push(Notice {
            turn: None,
            kind: "rules".into(),
            text: text.into(),
        });
    }
    if let JudgeGate::Disabled { note: Some(note) } = gate {
        notices.push(Notice {
            turn: None,
            kind: "rules".into(),
            text: note.into(),
        });
    }
    let state: Arc<dyn Any + Send + Sync> = Arc::new(TtsrSessionState {
        rules,
        gate: repeat_gate,
        judge,
    });
    Ok(SessionInstalls {
        watches: vec![Arc::new(factory)],
        notices,
        state: Some(state),
    })
}

fn rules_for_session(cx: &SessionCx) -> RuleSet {
    let agent = if cx.parent.is_none() { "main" } else { "sub" };
    if let Some(parent) = cx
        .inherited_state
        .as_ref()
        .and_then(|state| state.downcast_ref::<TtsrSessionState>())
    {
        return for_agent(&parent.rules, agent);
    }
    let records: Vec<RecordSource> = cx
        .rules
        .iter()
        .map(|source| RecordSource {
            plugin: source.ext.clone(),
            site: source.site.clone(),
            record: source.record.clone(),
        })
        .collect();
    let tool_names: Vec<&str> = cx.tool_names.iter().map(AsRef::as_ref).collect();
    let input = RuleBuildInput {
        records: &records,
        plugin_rules: &cx.rule_files,
        known_tools: &tool_names,
        agent,
    };
    set_for_with_judged(
        &input,
        &cx.data_root,
        cx.workspace.as_path(),
        cx.config.rules(),
        true,
    )
}

#[derive(Debug)]
struct RulePrompt;

impl SectionFn for RulePrompt {
    fn render(&self, cx: &SectionCx<'_>) -> Option<String> {
        let state = cx.state?.downcast_ref::<TtsrSessionState>()?;
        let always = state.rules.always_text();
        let rulebook = state.rules.rulebook_text();
        match (always.is_empty(), rulebook.is_empty()) {
            (true, true) => None,
            (false, true) => Some(always),
            (true, false) => Some(rulebook),
            (false, false) => Some(format!("{always}\n\n{rulebook}")),
        }
    }
}

/// The text `rule://<name>` serves: the file's original bytes for a rule that
/// came from disk, else the rule's body for a rule registered in code, such
/// as a bundled pack rule. A name outside the loaded set returns `None`.
fn rule_source(rules: &RuleSet, name: &str) -> Option<Vec<u8>> {
    rules
        .source_bytes(name)
        .map(Vec::from)
        .or_else(|| rules.body_of(name).map(|body| body.as_bytes().to_vec()))
}

#[derive(Debug)]
struct RuleResolver;

impl SchemeResolver for RuleResolver {
    fn read<'a>(
        &'a self,
        path: &'a str,
        cx: &'a SchemeCx<'a>,
    ) -> BoxFuture<'a, Result<dal_agent::ext::Doc, SchemeError>> {
        let result = cx
            .session_state()
            .and_then(|state| state.downcast_ref::<TtsrSessionState>())
            .and_then(|state| rule_source(&state.rules, path));
        Box::pin(async move {
            let Some(bytes) = result else {
                return Err(SchemeError::NotFound {
                    uri: format!("rule://{path}").into(),
                });
            };
            let text = String::from_utf8(bytes).map_err(|_| SchemeError::Failed {
                message: format!("rule://{path} is not valid UTF-8").into(),
            })?;
            Ok(dal_agent::ext::Doc::new(format!("rule://{path}"), text))
        })
    }
}

/// Builds the session-owned TTSR extension.
///
/// # Errors
///
/// Returns [`RegistrationError`] when the host rejects the extension or one of
/// its owned prompt, setup, or scheme registrations.
pub fn extension(deps: TtsrDeps) -> Result<Extension, RegistrationError> {
    ExtensionBuilder::new("ttsr", env!("CARGO_PKG_VERSION"), ServiceSet::EMPTY)?
        .session_setup(TtsrSetup { deps })
        .prompt_section(PromptSection::session(
            PromptOrder::Rules,
            Arc::new(RulePrompt),
        ))
        .scheme("rule", Arc::new(RuleResolver))
        .build()
}

#[cfg(test)]
mod tests;
