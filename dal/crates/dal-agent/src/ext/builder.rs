//! Extension registration: the builder and the sealed extension it produces.

use std::sync::{Arc, Mutex};

use dal_core::ext::{BeforeRequest, BeforeTurn, ExportId, ExportKind, OpId, TurnEnd};
use dal_core::{
    Claimant, CommandSpec, InputEvent, InputVerdict, Name, Origin, RegistrationError,
    RequestParams, RuleRecord, ServiceSet, SessionEnd, SessionStart, Settled, Site, SkillRecord,
    ToolCallEvent, ToolCallVerdict, ToolResultEvent, Visibility, valid_version,
};

use super::docs::{self, DocRecord};
use super::generation::catalog::{ExportSpec, wire_name};
use super::prompt::PromptSection;
use super::script::Evidence;
use super::{
    Attach, CommandHandler, Compactor, Hook, McpClient, ModelRecord, ObserveHook, SchemeResolver,
    StatusPoll, Tool, WatchFactory,
};

/// The export rejection naming the entry and its expected wire name.
fn invalid_export(id: &ExportId, expected: &str) -> RegistrationError {
    RegistrationError::InvalidExport {
        id: OpId::Export(id.clone()).to_string().into(),
        wire: expected.into(),
    }
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

/// Collects one extension's registrations before validation.
pub struct ExtensionBuilder {
    name: Box<str>,
    version: Box<str>,
    inject: ServiceSet,
    origin: Origin,
    site: Option<Site>,
    tools: Vec<(Arc<dyn Tool>, Visibility)>,
    commands: Vec<(CommandSpec, Arc<dyn CommandHandler>)>,
    skills: Vec<SkillRecord>,
    rules: Vec<RuleRecord>,
    prompt_sections: Vec<PromptSection>,
    session_starts: Vec<Arc<dyn ObserveHook<SessionStart>>>,
    session_starts_lossless: Vec<Arc<dyn ObserveHook<SessionStart>>>,
    session_ends: Vec<Arc<dyn ObserveHook<SessionEnd>>>,
    session_ends_lossless: Vec<Arc<dyn ObserveHook<SessionEnd>>>,
    inputs: Vec<Arc<dyn Hook<InputEvent, InputVerdict>>>,
    before_turns: Vec<Arc<dyn Hook<BeforeTurn, Option<String>>>>,
    before_requests: Vec<Arc<dyn Hook<BeforeRequest, Option<RequestParams>>>>,
    tool_calls: Vec<Arc<dyn Hook<ToolCallEvent, ToolCallVerdict>>>,
    tool_results: Vec<Arc<dyn ObserveHook<ToolResultEvent>>>,
    tool_results_lossless: Vec<Arc<dyn ObserveHook<ToolResultEvent>>>,
    turn_ends: Vec<Arc<dyn ObserveHook<TurnEnd>>>,
    turn_ends_lossless: Vec<Arc<dyn ObserveHook<TurnEnd>>>,
    settleds: Vec<Arc<dyn ObserveHook<Settled>>>,
    settleds_lossless: Vec<Arc<dyn ObserveHook<Settled>>>,
    watches: Vec<Arc<dyn WatchFactory>>,
    schemes: Vec<(Box<str>, Arc<dyn SchemeResolver>)>,
    compactors: Vec<(Box<str>, Arc<dyn Compactor>)>,
    mcp_clients: Vec<Arc<dyn McpClient>>,
    attaches: Vec<Attach>,
    statuses: Vec<(Box<str>, Arc<dyn StatusPoll>)>,
    models: Vec<ModelRecord>,
    evidence: Vec<Arc<dyn Evidence>>,
    docs: Vec<DocRecord>,
    exports: Vec<(ExportSpec, Name)>,
    declared_exports: Vec<ExportSpec>,
}

impl ExtensionBuilder {
    /// Starts a builder for extension `name` at `version` with injected services.
    ///
    /// # Errors
    /// Returns [`RegistrationError::InvalidName`] or
    /// [`RegistrationError::InvalidVersion`] for a malformed identity.
    pub fn new(name: &str, version: &str, inject: ServiceSet) -> Result<Self, RegistrationError> {
        if !valid_name(name) {
            return Err(RegistrationError::InvalidName { name: name.into() });
        }
        if !valid_version(version) {
            return Err(RegistrationError::InvalidVersion {
                version: version.into(),
            });
        }
        Ok(Self {
            name: name.into(),
            version: version.into(),
            inject,
            origin: Origin::Builtin,
            site: None,
            tools: Vec::new(),
            commands: Vec::new(),
            skills: Vec::new(),
            rules: Vec::new(),
            prompt_sections: Vec::new(),
            session_starts: Vec::new(),
            session_starts_lossless: Vec::new(),
            session_ends: Vec::new(),
            session_ends_lossless: Vec::new(),
            inputs: Vec::new(),
            before_turns: Vec::new(),
            before_requests: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            tool_results_lossless: Vec::new(),
            turn_ends: Vec::new(),
            turn_ends_lossless: Vec::new(),
            settleds: Vec::new(),
            settleds_lossless: Vec::new(),
            watches: Vec::new(),
            schemes: Vec::new(),
            compactors: Vec::new(),
            mcp_clients: Vec::new(),
            attaches: Vec::new(),
            statuses: Vec::new(),
            models: Vec::new(),
            evidence: Vec::new(),
            docs: Vec::new(),
            exports: Vec::new(),
            declared_exports: Vec::new(),
        })
    }
    /// Sets the extension origin and its source site.
    #[must_use]
    pub fn with_origin(mut self, origin: Origin, site: Option<Site>) -> Self {
        self.origin = origin;
        self.site = site;
        self
    }
    /// Registers one model-callable tool with its visibility.
    #[must_use]
    pub fn tool(mut self, tool: Arc<dyn Tool>, visibility: Visibility) -> Self {
        self.tools.push((tool, visibility));
        self
    }
    /// Registers a scripted tool export whose wire name is the tool's name (R02 R03).
    ///
    /// [`ExtensionBuilder::build`] rejects the export unless it is a tool of
    /// this extension and the tool is named [`wire_name`] of its identity.
    #[must_use]
    pub fn script_tool(
        mut self,
        tool: Arc<dyn Tool>,
        visibility: Visibility,
        export: ExportSpec,
    ) -> Self {
        self.exports.push((export, tool.name().clone()));
        self.tools.push((tool, visibility));
        self
    }
    /// Registers one scripted command with its handler and its declaration (R02 R03 R04).
    ///
    /// The declaration's wire name is [`wire_name`] of its identity;
    /// [`ExtensionBuilder::build`] rejects the export unless its identity
    /// names this extension.
    #[must_use]
    pub fn script_command(
        mut self,
        spec: CommandSpec,
        handler: Arc<dyn CommandHandler>,
        export: ExportSpec,
    ) -> Self {
        self.declared_exports.push(export);
        self.commands.push((spec, handler));
        self
    }
    /// Records one scripted hook's declaration (R02 R03 R04).
    ///
    /// The subscription itself is registered through the `on_*` methods;
    /// the declaration's wire name is [`wire_name`] of its identity and
    /// [`ExtensionBuilder::build`] rejects the export unless its identity
    /// names this extension.
    #[must_use]
    pub fn hook_export(mut self, export: ExportSpec) -> Self {
        self.declared_exports.push(export);
        self
    }
    /// Registers one command with its handler.
    #[must_use]
    pub fn command(mut self, spec: CommandSpec, handler: Arc<dyn CommandHandler>) -> Self {
        self.commands.push((spec, handler));
        self
    }
    /// Registers one skill record.
    #[must_use]
    pub fn skill(mut self, skill: SkillRecord) -> Self {
        self.skills.push(skill);
        self
    }
    /// Registers one rule record.
    #[must_use]
    pub fn rule(mut self, rule: RuleRecord) -> Self {
        self.rules.push(rule);
        self
    }
    /// Registers the extension's prompt section; at most one is allowed.
    #[must_use]
    pub fn prompt_section(mut self, section: PromptSection) -> Self {
        self.prompt_sections.push(section);
        self
    }
    /// Observes session starts.
    #[must_use]
    pub fn on_session_start(mut self, f: impl ObserveHook<SessionStart>) -> Self {
        self.session_starts.push(Arc::new(f));
        self
    }
    /// Observes session starts without dropping events under backpressure.
    #[must_use]
    pub fn on_session_start_lossless(mut self, f: impl ObserveHook<SessionStart>) -> Self {
        self.session_starts_lossless.push(Arc::new(f));
        self
    }
    /// Observes session ends.
    #[must_use]
    pub fn on_session_end(mut self, f: impl ObserveHook<SessionEnd>) -> Self {
        self.session_ends.push(Arc::new(f));
        self
    }
    /// Observes session ends without dropping events under backpressure.
    #[must_use]
    pub fn on_session_end_lossless(mut self, f: impl ObserveHook<SessionEnd>) -> Self {
        self.session_ends_lossless.push(Arc::new(f));
        self
    }
    /// Guards user input with a verdict.
    #[must_use]
    pub fn on_input(mut self, f: impl Hook<InputEvent, InputVerdict>) -> Self {
        self.inputs.push(Arc::new(f));
        self
    }
    /// Runs before each turn and may contribute turn text.
    #[must_use]
    pub fn on_before_turn(mut self, f: impl Hook<BeforeTurn, Option<String>>) -> Self {
        self.before_turns.push(Arc::new(f));
        self
    }
    /// Runs before each provider request and may adjust its parameters.
    #[must_use]
    pub fn on_before_request(mut self, f: impl Hook<BeforeRequest, Option<RequestParams>>) -> Self {
        self.before_requests.push(Arc::new(f));
        self
    }
    /// Guards tool calls with a verdict.
    #[must_use]
    pub fn on_tool_call(mut self, f: impl Hook<ToolCallEvent, ToolCallVerdict>) -> Self {
        self.tool_calls.push(Arc::new(f));
        self
    }
    /// Observes tool results.
    #[must_use]
    pub fn on_tool_result(mut self, f: impl ObserveHook<ToolResultEvent>) -> Self {
        self.tool_results.push(Arc::new(f));
        self
    }
    /// Observes tool results without dropping events under backpressure.
    #[must_use]
    pub fn on_tool_result_lossless(mut self, f: impl ObserveHook<ToolResultEvent>) -> Self {
        self.tool_results_lossless.push(Arc::new(f));
        self
    }
    /// Observes turn ends.
    #[must_use]
    pub fn on_turn_end(mut self, f: impl ObserveHook<TurnEnd>) -> Self {
        self.turn_ends.push(Arc::new(f));
        self
    }
    /// Observes turn ends without dropping events under backpressure.
    #[must_use]
    pub fn on_turn_end_lossless(mut self, f: impl ObserveHook<TurnEnd>) -> Self {
        self.turn_ends_lossless.push(Arc::new(f));
        self
    }
    /// Observes settled calls.
    #[must_use]
    pub fn on_settled(mut self, f: impl ObserveHook<Settled>) -> Self {
        self.settleds.push(Arc::new(f));
        self
    }
    /// Observes settled calls without dropping events under backpressure.
    #[must_use]
    pub fn on_settled_lossless(mut self, f: impl ObserveHook<Settled>) -> Self {
        self.settleds_lossless.push(Arc::new(f));
        self
    }
    /// Registers a stream watcher factory.
    #[must_use]
    pub fn output_stream(mut self, f: Arc<dyn WatchFactory>) -> Self {
        self.watches.push(f);
        self
    }
    /// Registers a URI scheme resolver under `name`.
    #[must_use]
    pub fn scheme(mut self, name: &str, resolver: Arc<dyn SchemeResolver>) -> Self {
        self.schemes.push((name.into(), resolver));
        self
    }
    /// Registers a named compactor.
    #[must_use]
    pub fn compactor(mut self, name: &str, compactor: Arc<dyn Compactor>) -> Self {
        self.compactors.push((name.into(), compactor));
        self
    }
    /// Registers the MCP client; a generation holds at most one.
    #[must_use]
    pub fn mcp_client(mut self, client: Arc<dyn McpClient>) -> Self {
        self.mcp_clients.push(client);
        self
    }
    /// Registers one attachment.
    #[must_use]
    pub fn attach(mut self, attach: Attach) -> Self {
        self.attaches.push(attach);
        self
    }
    /// Registers the status kind and its poll; at most one is allowed.
    #[must_use]
    pub fn status_kind(mut self, kind: &str, poll: Arc<dyn StatusPoll>) -> Self {
        self.statuses.push((kind.into(), poll));
        self
    }
    /// Registers one model with its handler.
    #[must_use]
    pub fn model(mut self, record: ModelRecord) -> Self {
        self.models.push(record);
        self
    }
    /// Installs the evidence owner the host delivers views through (R06 E06).
    #[must_use]
    pub fn evidence(mut self, owner: Arc<dyn Evidence>) -> Self {
        self.evidence.push(owner);
        self
    }
    /// Registers one doc page, served as `<name>://<path>`.
    ///
    /// The scheme is the extension name; it and `path` are validated in
    /// [`ExtensionBuilder::build`]. The call site is recorded for spans.
    #[track_caller]
    #[must_use]
    pub fn doc(mut self, path: &str, title: &str, text: &str) -> Self {
        let here = std::panic::Location::caller();
        let site = Site {
            path: here.file().into(),
            line: here.line(),
            col: here.column(),
        };
        self.docs.push(DocRecord::capture(path, title, text, site));
        self
    }
    /// Validates the registrations and seals the extension.
    ///
    /// # Errors
    /// Returns a [`RegistrationError`] for an invalid identity, a duplicate
    /// skill, rule, prompt section or status kind, an invalid export, or an
    /// invalid doc scheme or page.
    #[expect(
        clippy::too_many_lines,
        reason = "Ordered validation reports the first registration error before sealing."
    )]
    pub fn build(self) -> Result<Extension, RegistrationError> {
        if !valid_name(&self.name) {
            return Err(RegistrationError::InvalidName { name: self.name });
        }
        if !valid_version(&self.version) {
            return Err(RegistrationError::InvalidVersion {
                version: self.version,
            });
        }
        let owner: Name = self.name.parse()?;
        for (index, skill) in self.skills.iter().enumerate() {
            if self.skills[..index]
                .iter()
                .any(|seen| seen.name == skill.name)
            {
                return Err(RegistrationError::Conflict {
                    kind: "skill",
                    name: skill.name.clone(),
                    claimant: Claimant::Plugin(owner.clone()),
                });
            }
        }
        for (index, rule) in self.rules.iter().enumerate() {
            if self.rules[..index]
                .iter()
                .any(|seen| seen.name == rule.name)
            {
                return Err(RegistrationError::Conflict {
                    kind: "rule",
                    name: rule.name.clone(),
                    claimant: Claimant::Plugin(owner.clone()),
                });
            }
        }
        if self.prompt_sections.len() > 1 {
            return Err(RegistrationError::DuplicatePromptSection);
        }
        if self.statuses.len() > 1 {
            return Err(RegistrationError::DuplicateStatusKind { ext: owner.clone() });
        }
        let mut exports = self.exports;
        let mut seen_exports: std::collections::BTreeSet<ExportId> =
            std::collections::BTreeSet::new();
        for (export, wire) in &exports {
            let expected = wire_name(&export.id);
            if export.id.kind != ExportKind::Tool
                || export.id.plugin != owner
                || wire.as_str() != expected
                || !seen_exports.insert(export.id.clone())
            {
                return Err(RegistrationError::InvalidExport {
                    id: OpId::Export(export.id.clone()).to_string().into(),
                    wire: expected.into(),
                });
            }
        }
        for export in self.declared_exports {
            let expected = wire_name(&export.id);
            let declared = matches!(export.id.kind, ExportKind::Command | ExportKind::Hook)
                && export.id.plugin == owner;
            let Some(Ok(wire)) = declared.then(|| Name::parse(&expected)) else {
                return Err(invalid_export(&export.id, &expected));
            };
            if !seen_exports.insert(export.id.clone()) {
                return Err(invalid_export(&export.id, &expected));
            }
            exports.push((export, wire));
        }
        for model in &self.models {
            let Some(export) = &model.export else {
                continue;
            };
            let expected = wire_name(export);
            if export.kind != ExportKind::Model
                || export.plugin != owner
                || !seen_exports.insert(export.clone())
            {
                return Err(invalid_export(export, &expected));
            }
        }
        if !self.docs.is_empty() {
            let site = self.docs[0].site.clone();
            docs::check_scheme(&self.name, &site)?;
            for record in &self.docs {
                docs::check_path(record)?;
            }
        }
        Ok(Extension {
            name: self.name,
            version: self.version,
            inject: self.inject,
            origin: self.origin,
            site: self.site,
            tools: self.tools,
            commands: self.commands,
            skills: self.skills,
            rules: self.rules,
            prompt_sections: self.prompt_sections,
            session_starts: self.session_starts,
            session_starts_lossless: self.session_starts_lossless,
            session_ends: self.session_ends,
            session_ends_lossless: self.session_ends_lossless,
            inputs: self.inputs,
            before_turns: self.before_turns,
            before_requests: self.before_requests,
            tool_calls: self.tool_calls,
            tool_results: self.tool_results,
            tool_results_lossless: self.tool_results_lossless,
            turn_ends: self.turn_ends,
            turn_ends_lossless: self.turn_ends_lossless,
            settleds: self.settleds,
            settleds_lossless: self.settleds_lossless,
            watches: self.watches,
            schemes: self.schemes,
            compactors: self.compactors,
            mcp_clients: self.mcp_clients,
            attaches: Arc::new(Mutex::new(self.attaches)),
            statuses: self.statuses,
            models: self.models,
            evidence: self.evidence,
            docs: self.docs,
            exports,
        })
    }
}

/// One validated extension: its identity and every registration.
#[derive(Clone)]
pub struct Extension {
    pub(super) name: Box<str>,
    pub(super) version: Box<str>,
    pub(super) inject: ServiceSet,
    pub(super) origin: Origin,
    pub(super) site: Option<Site>,
    pub(super) tools: Vec<(Arc<dyn Tool>, Visibility)>,
    pub(super) commands: Vec<(CommandSpec, Arc<dyn CommandHandler>)>,
    pub(super) skills: Vec<SkillRecord>,
    pub(super) rules: Vec<RuleRecord>,
    pub(super) prompt_sections: Vec<PromptSection>,
    pub(super) session_starts: Vec<Arc<dyn ObserveHook<SessionStart>>>,
    pub(super) session_starts_lossless: Vec<Arc<dyn ObserveHook<SessionStart>>>,
    pub(super) session_ends: Vec<Arc<dyn ObserveHook<SessionEnd>>>,
    pub(super) session_ends_lossless: Vec<Arc<dyn ObserveHook<SessionEnd>>>,
    pub(super) inputs: Vec<Arc<dyn Hook<InputEvent, InputVerdict>>>,
    pub(super) before_turns: Vec<Arc<dyn Hook<BeforeTurn, Option<String>>>>,
    pub(super) before_requests: Vec<Arc<dyn Hook<BeforeRequest, Option<RequestParams>>>>,
    pub(super) tool_calls: Vec<Arc<dyn Hook<ToolCallEvent, ToolCallVerdict>>>,
    pub(super) tool_results: Vec<Arc<dyn ObserveHook<ToolResultEvent>>>,
    pub(super) tool_results_lossless: Vec<Arc<dyn ObserveHook<ToolResultEvent>>>,
    pub(super) turn_ends: Vec<Arc<dyn ObserveHook<TurnEnd>>>,
    pub(super) turn_ends_lossless: Vec<Arc<dyn ObserveHook<TurnEnd>>>,
    pub(super) settleds: Vec<Arc<dyn ObserveHook<Settled>>>,
    pub(super) settleds_lossless: Vec<Arc<dyn ObserveHook<Settled>>>,
    pub(super) watches: Vec<Arc<dyn WatchFactory>>,
    pub(super) schemes: Vec<(Box<str>, Arc<dyn SchemeResolver>)>,
    pub(super) compactors: Vec<(Box<str>, Arc<dyn Compactor>)>,
    pub(super) mcp_clients: Vec<Arc<dyn McpClient>>,
    pub(super) attaches: Arc<Mutex<Vec<Attach>>>,
    pub(super) statuses: Vec<(Box<str>, Arc<dyn StatusPoll>)>,
    pub(super) models: Vec<ModelRecord>,
    pub(super) evidence: Vec<Arc<dyn Evidence>>,
    pub(super) docs: Vec<DocRecord>,
    pub(super) exports: Vec<(ExportSpec, Name)>,
}

impl Extension {
    /// Returns the extension name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Returns the extension version.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Returns the injected service set.
    #[must_use]
    pub fn inject(&self) -> ServiceSet {
        self.inject
    }
    /// Returns where the extension came from.
    #[must_use]
    pub fn origin(&self) -> Origin {
        self.origin
    }
    /// Borrows the source site, when known.
    #[must_use]
    pub fn site(&self) -> Option<&Site> {
        self.site.as_ref()
    }
    /// Borrows the registered tools with their visibility.
    #[must_use]
    pub fn tools(&self) -> &[(Arc<dyn Tool>, Visibility)] {
        &self.tools
    }
    /// Borrows the registered commands with their handlers.
    #[must_use]
    pub fn commands(&self) -> &[(CommandSpec, Arc<dyn CommandHandler>)] {
        &self.commands
    }
    /// Borrows the registered skills.
    #[must_use]
    pub fn skills(&self) -> &[SkillRecord] {
        &self.skills
    }
    /// Borrows the registered rules.
    #[must_use]
    pub fn rules(&self) -> &[RuleRecord] {
        &self.rules
    }
    /// Borrows the prompt section, when registered.
    #[must_use]
    pub fn prompt_section(&self) -> Option<&PromptSection> {
        self.prompt_sections.first()
    }
    /// Borrows the status kind and its poll, when registered.
    #[must_use]
    pub fn status(&self) -> Option<(&str, &Arc<dyn StatusPoll>)> {
        self.statuses.first().map(|entry| (&*entry.0, &entry.1))
    }
    /// Borrows the registered models.
    #[must_use]
    pub fn models(&self) -> &[ModelRecord] {
        &self.models
    }
    /// Borrows the registered scheme resolvers.
    #[must_use]
    pub fn schemes(&self) -> &[(Box<str>, Arc<dyn SchemeResolver>)] {
        &self.schemes
    }
    /// Borrows the registered compactors.
    #[must_use]
    pub fn compactors(&self) -> &[(Box<str>, Arc<dyn Compactor>)] {
        &self.compactors
    }
    /// Borrows the registered MCP clients.
    #[must_use]
    pub fn mcp_clients(&self) -> &[Arc<dyn McpClient>] {
        &self.mcp_clients
    }
    /// Hands the registered attachments to the host exactly once; later
    /// calls, on this extension or any clone of it, return nothing.
    #[must_use]
    pub(crate) fn take_attaches(&self) -> Vec<Attach> {
        std::mem::take(
            &mut *self
                .attaches
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
    /// Borrows the registered doc records.
    #[must_use]
    pub fn docs(&self) -> &[DocRecord] {
        &self.docs
    }
    /// Borrows the evidence owners this extension installs.
    #[must_use]
    pub fn evidence(&self) -> &[Arc<dyn Evidence>] {
        &self.evidence
    }
    /// Borrows the scripted exports with their provider-wire tool names.
    #[must_use]
    pub fn exports(&self) -> &[(ExportSpec, Name)] {
        &self.exports
    }
}

impl std::fmt::Debug for Extension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Extension")
            .field("name", &self.name)
            .field("version", &self.version)
            .field("origin", &self.origin)
            .field("tools", &self.tools.len())
            .field("commands", &self.commands.len())
            .field("skills", &self.skills.len())
            .field("rules", &self.rules.len())
            .field("statuses", &self.statuses.len())
            .field("models", &self.models.len())
            .finish_non_exhaustive()
    }
}
