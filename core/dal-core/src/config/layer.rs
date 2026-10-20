use super::limits::parse_plugin_limits;
use super::parse::parse_layer;
use super::partial::{
    ParsedLayer, PartialGuard, PartialGuardBands, PartialGuardPolicies, PartialPrice,
};
use super::prices::finish_prices;
use super::values::parse_sandbox_writable;
use super::{
    AgentsConfig, ApprovalMode, AskTimeoutClamp, Config, ConfigError, ConfigOverrides,
    ConfigProduct, EditStyleInput, GuardBands, GuardPolicies, GuardSection, Mode, PluginLimits,
    RulesConfig, Screen, ServeConfig, TuiConfig, agents, ask, eval::EvalConfig, invalid_value,
};
use crate::ext::OpSet;

impl Config {
    /// Loads configuration without accessing the filesystem or environment.
    ///
    /// Layers are applied in order: built-in defaults, compiled product
    /// defaults, then user TOML.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when any layer has a syntax error, an unknown
    /// key, or an invalid value, or when the compiled product defaults are
    /// corrupt.
    pub fn load(
        product: ConfigProduct,
        data_root: &std::path::Path,
        product_defaults_toml: &str,
        user_toml: Option<&str>,
    ) -> Result<Self, ConfigError> {
        let mut config = Self::builtins(product, data_root);
        let mut prices = std::collections::BTreeMap::new();
        let mut serve_approval = None;
        let defaults = parse_layer(product, product_defaults_toml).map_err(|error| {
            ConfigError::DefaultsInvalid {
                message: format!("{error:?}").into_boxed_str(),
            }
        })?;
        config.apply_layer(defaults, &mut prices, &mut serve_approval);
        if let Some(user_toml) = user_toml {
            let user = parse_layer(product, user_toml)?;
            config.apply_layer(user, &mut prices, &mut serve_approval);
        }
        config.serve.approval = serve_approval.unwrap_or(config.approval);
        config.prices = finish_prices(prices)?;
        config.plugin_limits = parse_plugin_limits(&config.sections)?;
        config.agents = agents::parse_agents(&config.sections)?;
        let (ask, ask_clamp) = ask::parse_ask(&config.sections)?;
        config.ask = ask;
        config.ask_clamp = ask_clamp;
        config.sandbox_writable = parse_sandbox_writable(&config.sections)?;
        Ok(config)
    }

    /// Reload-validates a rendered user TOML document without touching live state.
    ///
    /// Parses and validates exactly as [`Config::load`] would, discarding the
    /// result; the caller keeps its scratch snapshot only when this succeeds.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the document has a syntax error, an unknown
    /// key, or an invalid value.
    pub fn validate_user_toml(product: ConfigProduct, user_toml: &str) -> Result<(), ConfigError> {
        Self::load(product, std::path::Path::new(""), "", Some(user_toml)).map(|_| ())
    }

    /// Returns a copy with the supplied command-line flag overrides applied.
    #[must_use]
    pub fn with_overrides(&self, overrides: &ConfigOverrides) -> Self {
        let mut config = self.clone();
        if let Some(model) = &overrides.model {
            config.model = Some(model.clone());
            // A command-line model flag outranks the file: it names a bare
            // string with no structured family, so the file's structured
            // route must not shadow it in `default_model_route`.
            config.model_route = None;
        }
        if let Some(mode) = overrides.mode {
            config.mode = mode;
        }
        if let Some(thinking) = overrides.thinking {
            config.thinking = thinking;
        }
        if let Some(approval) = overrides.approval {
            config.approval = approval;
        }
        if let Some(screen) = overrides.screen {
            config.screen = screen;
        }
        if let Some(sandbox) = overrides.sandbox {
            config.sandbox = sandbox;
        }
        config
    }

    /// Checks configured battery names after the product registry is loaded.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when a configured battery name is not in
    /// `registered`.
    pub fn validate_battery_names(&self, registered: &[&str]) -> Result<(), ConfigError> {
        for (key, names) in [
            ("disabled_batteries", &self.disabled_batteries),
            ("experimental_batteries", &self.experimental_batteries),
        ] {
            for name in names {
                if !registered
                    .iter()
                    .any(|registered_name| *registered_name == name.as_ref())
                {
                    return Err(invalid_value(
                        key,
                        Box::<str>::from(name.as_ref()),
                        &format!("registered battery names: {}", registered.join(", ")),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Returns the configured price rates for a model, if present.
    #[must_use]
    pub fn price_for_model(&self, model_id: &str) -> Option<&crate::model::ModelPrice> {
        self.prices.get(model_id)
    }

    /// Returns the configured execution mode.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Returns the configured model identifier, if any.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Returns the authoritative structured default route, when the user
    /// configuration sets the `model_route` table.
    #[must_use]
    pub fn model_route(&self) -> Option<&crate::model::ModelRoute> {
        self.model_route.as_ref()
    }

    /// Returns the effective default route: the structured `model_route`
    /// table when present, else the legacy `model` string decoded with
    /// [`ModelRoute::from_id`], else nothing.
    ///
    /// [`ModelRoute::from_id`]: crate::model::ModelRoute::from_id
    #[must_use]
    pub fn default_model_route(&self) -> Option<crate::model::ModelRoute> {
        if let Some(route) = self.model_route.clone() {
            return Some(route);
        }
        self.model.as_deref().map(crate::model::ModelRoute::from_id)
    }

    /// Returns the configured reasoning level.
    #[must_use]
    pub fn thinking(&self) -> crate::model::ThinkingLevel {
        self.thinking
    }

    /// Returns the configured approval policy.
    #[must_use]
    pub fn approval(&self) -> ApprovalMode {
        self.approval
    }

    /// Returns the configured terminal screen.
    #[must_use]
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// Returns the effective `[tui]` configuration.
    #[must_use]
    pub const fn tui(&self) -> &TuiConfig {
        &self.tui
    }

    /// Updates the user TOML with the TUI diagram preference.
    ///
    /// The existing TOML document is edited in place so unrelated settings,
    /// extension tables, and comments remain intact.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the document is invalid or `[tui]` is not
    /// a table.
    pub fn update_tui_diagrams(
        &mut self,
        enabled: bool,
        user_toml: Option<&str>,
    ) -> Result<String, ConfigError> {
        let mut document = user_toml
            .unwrap_or_default()
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| ConfigError::Syntax {
                line: None,
                message: error.to_string().into_boxed_str(),
            })?;
        if !document.contains_key("tui") {
            document.insert("tui", toml_edit::Item::Table(toml_edit::Table::new()));
        }
        let tui = document
            .get_mut("tui")
            .and_then(toml_edit::Item::as_table_like_mut)
            .ok_or_else(|| invalid_value("tui", "non-table", "table"))?;
        match tui.get_mut("diagrams") {
            Some(item) => {
                let expected = item.to_string();
                let Some(value) = item.as_value_mut() else {
                    return Err(invalid_value("tui.diagrams", expected, "boolean"));
                };
                if value.as_bool().is_none() {
                    return Err(invalid_value("tui.diagrams", expected, "boolean"));
                }
                let decor = value.decor().clone();
                let mut replacement = toml_edit::Value::from(enabled);
                *replacement.decor_mut() = decor;
                *value = replacement;
            }
            None => {
                tui.insert("diagrams", toml_edit::value(enabled));
            }
        }
        self.tui.diagrams = enabled;
        Ok(document.to_string())
    }

    /// Updates the user TOML with the default execution mode.
    ///
    /// The existing TOML document is edited in place so unrelated settings,
    /// extension tables, and comments remain intact.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the document is invalid or `mode` is not
    /// a string.
    pub fn update_default_mode(
        &mut self,
        mode: Mode,
        user_toml: Option<&str>,
    ) -> Result<String, ConfigError> {
        let mut document = parse_user_document(user_toml)?;
        replace_top_string(
            &mut document,
            "mode",
            "normal, eval-first, eval-only",
            mode.as_str(),
        )?;
        self.mode = mode;
        Ok(document.to_string())
    }

    /// Updates the user TOML with the default reasoning level.
    ///
    /// The existing TOML document is edited in place so unrelated settings,
    /// extension tables, and comments remain intact.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the document is invalid or `thinking` is
    /// not a string.
    pub fn update_default_thinking(
        &mut self,
        level: crate::model::ThinkingLevel,
        user_toml: Option<&str>,
    ) -> Result<String, ConfigError> {
        let mut document = parse_user_document(user_toml)?;
        replace_top_string(
            &mut document,
            "thinking",
            "off, minimal, low, medium, high, xhigh, max",
            level.name(),
        )?;
        self.thinking = level;
        Ok(document.to_string())
    }

    /// Updates the user TOML with the default approval policy.
    ///
    /// The existing TOML document is edited in place so unrelated settings,
    /// extension tables, and comments remain intact.
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the document is invalid or `approval` is
    /// not a string.
    pub fn update_default_approval(
        &mut self,
        mode: ApprovalMode,
        user_toml: Option<&str>,
    ) -> Result<String, ConfigError> {
        let mut document = parse_user_document(user_toml)?;
        replace_top_string(
            &mut document,
            "approval",
            "ask, edits, all",
            approval_as_str(mode),
        )?;
        self.approval = mode;
        Ok(document.to_string())
    }

    /// Updates the user TOML with the default model route.
    ///
    /// The existing TOML document is edited in place so unrelated settings,
    /// extension tables, and comments remain intact. Both the legacy `model`
    /// string (the [`ModelRoute::family_qualified_reference`] mirror for old
    /// readers) and the authoritative `model_route` table are replaced
    /// atomically in the same document.
    ///
    /// An API route that names its provider also writes `provider` in the
    /// table; the legacy `model` string never carries it. A route without a
    /// provider writes exactly the table earlier versions wrote. A version
    /// from before `provider` existed refuses a table that has the member
    /// (its table decoder rejects unknown members), so a file saved with a
    /// provider route does not load there.
    ///
    /// [`ModelRoute::family_qualified_reference`]: crate::model::ModelRoute::family_qualified_reference
    ///
    /// # Errors
    /// Returns [`ConfigError`] when the document is invalid or `model` is not
    /// a string.
    pub fn update_default_model(
        &mut self,
        route: &crate::model::ModelRoute,
        user_toml: Option<&str>,
    ) -> Result<String, ConfigError> {
        let mut document = parse_user_document(user_toml)?;
        replace_top_string(
            &mut document,
            "model",
            "non-empty model id",
            &route.family_qualified_reference(),
        )?;
        replace_model_route_table(&mut document, route);
        self.model = Some(route.family_qualified_reference().into_boxed_str());
        self.model_route = Some(route.clone());
        Ok(document.to_string())
    }

    /// Returns settings for the local control endpoint.
    #[must_use]
    pub fn serve(&self) -> &ServeConfig {
        &self.serve
    }

    /// Returns the effective `[rules]` table.
    #[must_use]
    pub fn rules(&self) -> &RulesConfig {
        &self.rules
    }

    /// Returns the configured theme name.
    #[must_use]
    pub fn theme(&self) -> &str {
        &self.theme
    }

    /// Returns whether the sandbox is enabled.
    #[must_use]
    pub const fn sandbox(&self) -> bool {
        self.sandbox
    }

    /// Returns the configured sandbox writable roots in config order.
    #[must_use]
    pub fn sandbox_writable(&self) -> &[Box<str>] {
        &self.sandbox_writable
    }

    /// Returns whether images are enabled.
    #[must_use]
    pub const fn images(&self) -> bool {
        self.images
    }

    /// Returns whether motion is enabled.
    #[must_use]
    pub const fn motion(&self) -> bool {
        self.motion
    }

    /// Returns the automatic compaction ratio.
    #[must_use]
    pub const fn compact_ratio(&self) -> f64 {
        self.compact_ratio
    }

    /// Returns the ordered edit-style input.
    #[must_use]
    pub const fn edit_style(&self) -> &EditStyleInput {
        &self.edit_style
    }

    /// Returns the effective `[guard]` table.
    #[must_use]
    pub const fn guard(&self) -> &GuardSection {
        &self.guard
    }

    /// Returns whether symbol search is enabled.
    #[must_use]
    pub const fn search_symbols(&self) -> bool {
        self.search_symbols
    }

    /// Returns the configured plugin names.
    #[must_use]
    pub fn plugins(&self) -> &[Box<str>] {
        &self.plugins
    }
    /// Returns the `[limits.plugins]` Starlark budgets.
    #[must_use]
    pub const fn plugin_limits(&self) -> &PluginLimits {
        &self.plugin_limits
    }

    /// Returns the typed `[agents]` table.
    #[must_use]
    pub const fn agents(&self) -> &AgentsConfig {
        &self.agents
    }

    /// Returns the typed `[ask]` table.
    #[must_use]
    pub const fn ask(&self) -> &crate::config::AskConfig {
        &self.ask
    }

    /// Returns the clamped `[ask] timeout` startup notice, when the
    /// configured value was clamped at load.
    #[must_use]
    pub fn ask_clamp_notice(&self) -> Option<String> {
        self.ask_clamp.as_ref().map(AskTimeoutClamp::notice)
    }

    /// Returns the effective `[eval]` operation authority.
    ///
    /// Empty when unset: pure computation only, fail closed.
    #[must_use]
    pub fn eval_uses(&self) -> &OpSet {
        &self.eval.uses
    }

    /// Returns every merged `[plugin.<name>]` table by plugin name.
    ///
    /// The tables stay raw; each plugin validates its own table against its
    /// `config` schema when it loads.
    pub fn plugin_configs(&self) -> impl Iterator<Item = (&str, &toml::Value)> {
        self.sections
            .get("plugin")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flatten()
            .map(|(name, table)| (name.as_str(), table))
    }

    /// Returns the configured model aliases.
    #[must_use]
    pub fn aliases(&self) -> &std::collections::BTreeMap<Box<str>, Box<str>> {
        &self.aliases
    }

    /// Returns the configured disabled battery names.
    #[must_use]
    pub fn disabled_batteries(&self) -> &[Box<str>] {
        &self.disabled_batteries
    }

    /// Returns the configured experimental battery names.
    #[must_use]
    pub fn experimental_batteries(&self) -> &[Box<str>] {
        &self.experimental_batteries
    }

    /// Returns the merged raw TOML for a dotted section path.
    ///
    /// The path is split at `.`; `section("plugin.web")` returns the
    /// `[plugin.web]` table when present. Returns `None` for an absent path
    /// or when an intermediate member is not a table.
    #[must_use]
    pub fn section(&self, path: &str) -> Option<&toml::Value> {
        let mut current: Option<&toml::Value> = None;
        for (index, key) in path.split('.').enumerate() {
            if index == 0 {
                current = self.sections.get(key).map(|value| value as &toml::Value);
            } else {
                current = match current {
                    Some(toml::Value::Table(table)) => table.get(key),
                    _ => return None,
                };
            }
        }
        current
    }

    pub(super) fn builtins(product: ConfigProduct, data_root: &std::path::Path) -> Self {
        let (edit_style, guard_enabled, search_symbols) = match product {
            ConfigProduct::Dalgon => ("anchor", false, false),
            ConfigProduct::Dalgona => ("hashline", true, true),
        };
        let guard = GuardSection {
            enabled: guard_enabled,
            policies: GuardPolicies::default(),
        };
        Self {
            mode: Mode::Normal,
            model: None,
            model_route: None,
            thinking: crate::model::ThinkingLevel::Medium,
            approval: ApprovalMode::Ask,
            screen: Screen::Inline,
            theme: Box::<str>::from("auto"),
            sandbox: false,
            sandbox_writable: Vec::new(),
            images: false,
            motion: true,
            compact_ratio: 0.85,
            tui: TuiConfig::default(),
            edit_style: EditStyleInput::Scalar(Box::<str>::from(edit_style)),
            guard,
            search_symbols,
            plugins: Vec::new(),
            plugin_limits: PluginLimits::default(),
            agents: AgentsConfig::default(),
            ask: crate::config::AskConfig::default(),
            ask_clamp: None,
            aliases: std::collections::BTreeMap::new(),
            serve: ServeConfig {
                bind: Box::<str>::from(crate::config::types::SERVE_DEFAULT_BIND),
                port: 7437,
                token_file: data_root.join("serve.token"),
                approval: ApprovalMode::Ask,
                origins: Vec::new(),
            },
            prices: std::collections::BTreeMap::new(),
            disabled_batteries: Vec::new(),
            experimental_batteries: Vec::new(),
            rules: RulesConfig::default(),
            eval: EvalConfig::default(),
            sections: std::collections::BTreeMap::new(),
        }
    }

    pub(super) fn apply_layer(
        &mut self,
        mut layer: ParsedLayer,
        prices: &mut std::collections::BTreeMap<Box<str>, PartialPrice>,
        serve_approval: &mut Option<ApprovalMode>,
    ) {
        if let Some(value) = layer.mode.take() {
            self.mode = value;
        }
        if let Some(value) = layer.model.take() {
            self.model = Some(value);
        }
        if let Some(value) = layer.model_route.take() {
            self.model_route = Some(value);
        }
        if let Some(value) = layer.thinking.take() {
            self.thinking = value;
        }
        if let Some(value) = layer.approval.take() {
            self.approval = value;
        }
        if let Some(value) = layer.screen.take() {
            self.screen = value;
        }
        if let Some(value) = layer.tui.take() {
            self.tui = value;
        }
        replace_if_present(&mut self.theme, layer.theme.take());
        if let Some(value) = layer.sandbox.take() {
            self.sandbox = value;
        }
        if let Some(value) = layer.images.take() {
            self.images = value;
        }
        if let Some(value) = layer.motion.take() {
            self.motion = value;
        }
        if let Some(value) = layer.compact_ratio.take() {
            self.compact_ratio = value;
        }
        replace_if_present(&mut self.edit_style, layer.edit_style.take());
        if let Some(partial) = layer.guard.take() {
            apply_guard(&mut self.guard, partial);
        }
        if let Some(value) = layer.search_symbols.take() {
            self.search_symbols = value;
        }
        replace_if_present(&mut self.plugins, layer.plugins.take());
        replace_if_present(&mut self.aliases, layer.aliases.take());
        replace_if_present(
            &mut self.disabled_batteries,
            layer.disabled_batteries.take(),
        );
        replace_if_present(
            &mut self.experimental_batteries,
            layer.experimental_batteries.take(),
        );
        replace_if_present(&mut self.eval, layer.eval.take());
        if let Some(mut serve) = layer.serve.take() {
            replace_if_present(&mut self.serve.bind, serve.bind.take());
            if let Some(value) = serve.port.take() {
                self.serve.port = value;
            }
            replace_if_present(&mut self.serve.token_file, serve.token_file.take());
            if let Some(value) = serve.approval.take() {
                *serve_approval = Some(value);
            }
            replace_if_present(&mut self.serve.origins, serve.origins.take());
        }
        if let Some(rules) = layer.rules.take() {
            rules.apply_to(&mut self.rules);
        }
        for (model, later) in std::mem::take(&mut layer.prices) {
            match prices.entry(model) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(later);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().merge(&later);
                }
            }
        }
        for (name, later) in std::mem::take(&mut layer.sections) {
            match self.sections.get_mut(&name) {
                Some(current) => merge_toml_value(current, later),
                None => {
                    self.sections.insert(name, later);
                }
            }
        }
    }
}

/// Parses the user TOML into an editable document; a missing file edits as
/// an empty document so the first persisted default creates the file.
fn parse_user_document(user_toml: Option<&str>) -> Result<toml_edit::DocumentMut, ConfigError> {
    user_toml
        .unwrap_or_default()
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| ConfigError::Syntax {
            line: None,
            message: error.to_string().into_boxed_str(),
        })
}

/// Replaces a top-level string while keeping its leading decor (comments and
/// blank lines stay attached to the key). A missing key is inserted; a
/// present non-string is rejected instead of silently retyped.
fn replace_top_string(
    document: &mut toml_edit::DocumentMut,
    key: &str,
    expected: &str,
    rendered: &str,
) -> Result<(), ConfigError> {
    let Some(item) = document.get_mut(key) else {
        document.insert(key, toml_edit::value(rendered));
        return Ok(());
    };
    let Some(value) = item.as_value_mut() else {
        return Err(invalid_value(
            key,
            item.to_string().into_boxed_str(),
            expected,
        ));
    };
    if !matches!(value, toml_edit::Value::String(_)) {
        return Err(invalid_value(
            key,
            value.to_string().into_boxed_str(),
            expected,
        ));
    }
    let decor = value.decor().clone();
    let mut replacement = toml_edit::Value::from(rendered);
    *replacement.decor_mut() = decor;
    *value = replacement;
    Ok(())
}

/// Replaces the top-level `model_route` table wholesale with the structured
/// form of `route`. Sibling keys keep their position and decor; comments
/// inside a previous `model_route` table do not survive the replacement.
/// Stale kind members are dropped by construction: only the members of the
/// selected kind are written.
fn replace_model_route_table(
    document: &mut toml_edit::DocumentMut,
    route: &crate::model::ModelRoute,
) {
    use crate::model::ModelRoute;
    let mut table = toml_edit::Table::new();
    match route {
        ModelRoute::Api {
            family,
            model,
            provider,
        } => {
            table.insert("kind", toml_edit::value("api"));
            table.insert("family", toml_edit::value(family_wire(*family)));
            table.insert("model", toml_edit::value(model.as_ref()));
            // Provider-less routes keep their byte-identical file shape: the
            // member is written only when the route names its provider.
            if let Some(provider) = provider.as_ref() {
                table.insert("provider", toml_edit::value(provider.as_ref()));
            }
        }
        ModelRoute::Synthetic { id } => {
            table.insert("kind", toml_edit::value("synthetic"));
            table.insert("id", toml_edit::value(id.as_ref()));
        }
        ModelRoute::Harness { id } => {
            table.insert("kind", toml_edit::value("harness"));
            table.insert("id", toml_edit::value(id.as_ref()));
        }
    }
    document.insert("model_route", toml_edit::Item::Table(table));
}

/// Renders an approval policy with its canonical configuration spelling.
fn approval_as_str(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Ask => "ask",
        ApprovalMode::Edits => "edits",
        ApprovalMode::All => "all",
    }
}

/// Renders a provider family with its `model_route.family` wire name: the
/// [`crate::model::Family`] serde names, matching the
/// [`parse_model_route`](super::values::parse_model_route) grammar.
fn family_wire(family: crate::model::Family) -> &'static str {
    use crate::model::Family;
    match family {
        Family::Chat => "openai_chat",
        Family::Responses => "openai_responses",
        Family::Codex => "openai_codex",
        Family::Anthropic => "anthropic",
        Family::Bedrock => "bedrock",
        Family::Cursor => "cursor",
        Family::Devin => "devin",
        Family::Droid => "droid",
        Family::GitLab => "gitlab",
        Family::GoogleGenerativeAi => "google-generative-ai",
        Family::GoogleVertex => "google-vertex",
        Family::GoogleGeminiCli => "google-gemini-cli",
        Family::GoogleAntigravity => "google-antigravity",
        Family::Mistral => "mistral",
        Family::Ollama => "ollama",
        Family::PiMessages => "pi-messages",
    }
}

/// Merges a later TOML layer into the current value: tables merge by key,
/// every other value replaces the earlier one.
pub(super) fn merge_toml_value(current: &mut toml::Value, later: toml::Value) {
    match (current, later) {
        (toml::Value::Table(current), toml::Value::Table(later)) => {
            for (key, value) in later {
                match current.get_mut(&key) {
                    Some(current) => merge_toml_value(current, value),
                    None => {
                        current.insert(key, value);
                    }
                }
            }
        }
        (current, later) => *current = later,
    }
}
pub(super) fn replace_if_present<T>(target: &mut T, overlay: Option<T>) {
    if let Some(overlay) = overlay {
        *target = overlay;
    }
}
pub(super) fn apply_guard(target: &mut GuardSection, partial: PartialGuard) {
    if let Some(enabled) = partial.enabled {
        target.enabled = enabled;
    }
    if let Some(policies) = partial.policies {
        apply_guard_policies(&mut target.policies, policies);
    }
}

pub(super) fn apply_guard_policies(target: &mut GuardPolicies, partial: PartialGuardPolicies) {
    if let Some(bands) = partial.bands {
        apply_guard_bands(&mut target.bands, &bands);
    }
    if let Some(g2_mode) = partial.g2_mode {
        target.g2_mode = g2_mode;
    }
    if let Some(g3_mode) = partial.g3_mode {
        target.g3_mode = g3_mode;
    }
    if let Some(g4_enabled) = partial.g4_enabled {
        target.g4_enabled = g4_enabled;
    }
    if let Some(rules) = partial.g8_calibrated_rules {
        target.g8_calibrated_rules = rules;
    }
    if let Some(threshold) = partial.erosion_report_threshold {
        target.erosion_report_threshold = threshold;
    }
    if let Some(threshold) = partial.churn_turn_threshold {
        target.churn_turn_threshold = threshold;
    }
}

pub(super) fn apply_guard_bands(target: &mut GuardBands, partial: &PartialGuardBands) {
    if let Some(cognitive) = partial.cognitive {
        target.cognitive = cognitive;
    }
    if let Some(cyclomatic) = partial.cyclomatic {
        target.cyclomatic = cyclomatic;
    }
    if let Some(function_ploc) = partial.function_ploc {
        target.function_ploc = function_ploc;
    }
    if let Some(nesting) = partial.nesting {
        target.nesting = nesting;
    }
    if let Some(file_ploc) = partial.file_ploc {
        target.file_ploc = file_ploc;
    }
}
