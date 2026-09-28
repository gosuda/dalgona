//! Closed configuration policy values shared by dal's command and runtime layers.

use std::path::PathBuf;

/// The product execution mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Run a normal interaction.
    Normal,
    /// Run the evaluation pass before the interaction.
    EvalFirst,
    /// Run only the evaluation pass.
    EvalOnly,
}

impl Mode {
    /// Projects this mode to the two mutually exclusive runtime switches.
    #[must_use]
    pub fn projection(self) -> ModeProjection {
        match self {
            Self::Normal => ModeProjection {
                eval_first: false,
                non_eval_only: false,
            },
            Self::EvalFirst => ModeProjection {
                eval_first: true,
                non_eval_only: false,
            },
            Self::EvalOnly => ModeProjection {
                eval_first: false,
                non_eval_only: true,
            },
        }
    }

    /// Returns this mode's canonical configuration spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::EvalFirst => "eval-first",
            Self::EvalOnly => "eval-only",
        }
    }
}

/// Policy for requesting approval before a tool action.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// Ask before every action requiring approval.
    Ask,
    /// Automatically approve edits, but ask for other actions.
    Edits,
    /// Approve all actions.
    All,
}

/// Presentation mode for the terminal interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    /// Render inline in the existing terminal buffer.
    Inline,
    /// Use the terminal's fullscreen screen.
    Fullscreen,
}

/// Policy for whether and when judge evaluation is enabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum JudgeMode {
    /// Select judge behavior automatically.
    Auto,
    /// Enable judge evaluation.
    On,
    /// Disable judge evaluation.
    Off,
}

/// The product whose defaults are being loaded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ConfigProduct {
    /// The dalgon command-line product.
    Dalgon,
    /// The dalgona command-line product.
    Dalgona,
}

/// The runtime flags corresponding to a [`Mode`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ModeProjection {
    /// Whether to run the evaluation pass before the interaction.
    pub eval_first: bool,
    /// Whether execution is restricted to evaluation.
    pub non_eval_only: bool,
}

/// Configuration for serving the local control endpoint.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ServeConfig {
    /// Address on which the endpoint binds.
    pub bind: Box<str>,
    /// TCP port for the endpoint.
    pub port: u16,
    /// Path from which the endpoint's authentication token is read.
    pub token_file: PathBuf,
    /// Approval policy applied to endpoint actions.
    pub approval: ApprovalMode,
    /// Allowed browser origins.
    pub origins: Vec<Box<str>>,
}

/// The effective `[rules]` table read by the time-traveling stream rules.
///
/// Each key has a closed valid set; `Config::load` rejects any other value, so a
/// value of this type always satisfies the documented ranges.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct RulesConfig {
    /// Whether stream rules are watched at all.
    pub watch: bool,
    /// Interrupt mode for rules that set none of their own.
    pub interrupt: crate::ext::InterruptMode,
    /// Repeat mode for rules that set none of their own.
    pub repeat: crate::ext::RepeatMode,
    /// Turn gap for `after-gap` repeats, in `1..=1000`.
    pub repeat_gap: u16,
    /// Rule interrupts allowed per turn, in `0..=20`.
    pub max_retries: u32,
    /// Rule names dropped before rule sets are built; each matches
    /// `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
    pub disabled: Vec<Box<str>>,
    /// Gate for judged rules.
    pub judge: JudgeMode,
}

impl Default for RulesConfig {
    fn default() -> Self {
        Self {
            watch: true,
            interrupt: crate::ext::InterruptMode::Always,
            repeat: crate::ext::RepeatMode::Once,
            repeat_gap: 10,
            max_retries: 3,
            disabled: Vec::new(),
            judge: JudgeMode::Auto,
        }
    }
}

/// The fully resolved product configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent configuration switches have distinct merge and validation rules"
)]
pub struct Config {
    mode: Mode,
    model: Option<Box<str>>,
    thinking: crate::model::ThinkingLevel,
    approval: ApprovalMode,
    screen: Screen,
    theme: Box<str>,
    sandbox: bool,
    images: bool,
    motion: bool,
    compact_ratio: f64,
    edit_style: Box<str>,
    guard: bool,
    search_symbols: bool,
    judge: JudgeMode,
    plugins: Vec<Box<str>>,
    aliases: std::collections::BTreeMap<Box<str>, Box<str>>,
    serve: ServeConfig,
    prices: std::collections::BTreeMap<Box<str>, crate::model::ModelPrice>,
    disabled_batteries: Vec<Box<str>>,
    experimental_batteries: Vec<Box<str>>,
    rules: RulesConfig,
}

/// In-memory command-line overrides applied after file configuration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigOverrides {
    /// Optional model identifier override.
    pub model: Option<Box<str>>,
    /// Optional execution mode override.
    pub mode: Option<Mode>,
    /// Optional reasoning level override.
    pub thinking: Option<crate::model::ThinkingLevel>,
    /// Optional approval policy override.
    pub approval: Option<ApprovalMode>,
    /// Optional terminal screen override.
    pub screen: Option<Screen>,
    /// Optional sandbox override.
    pub sandbox: Option<bool>,
}

/// Errors produced while parsing or validating product configuration.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConfigError {
    /// The TOML document could not be parsed.
    #[error("config.toml syntax error")]
    Syntax {
        /// One-based source line when the TOML parser provides a span.
        line: Option<usize>,
        /// Parser diagnostic without its source-location prefix.
        message: Box<str>,
    },
    /// A key is not supported by the selected product or table.
    #[error("unknown config key: {key}")]
    UnknownKey {
        /// Dotted path of the unknown key.
        key: Box<str>,
        /// Closest supported key when it is within the suggestion distance.
        suggestion: Option<Box<str>>,
        /// Supported keys in their schema order.
        known_keys: Vec<Box<str>>,
    },
    /// A supported key has an invalid value.
    #[error("invalid config value for {key}")]
    InvalidValue {
        /// Dotted path of the invalid value.
        key: Box<str>,
        /// Text representation of the invalid value.
        value: Box<str>,
        /// Accepted value or constraint.
        expected: Box<str>,
    },
    /// Compiled product defaults are invalid.
    #[error("invalid product defaults")]
    DefaultsInvalid {
        /// Internal detail describing the invalid compiled defaults.
        message: Box<str>,
    },
}

impl ConfigError {
    /// Returns the two exact diagnostic lines of an invalid `[rules]` value, or
    /// `None` for every other error.
    ///
    /// `program` is the binary name, `dalgon` or `dalgona`. The first line names
    /// the key and the value in TOML form; the second line says what to write.
    #[must_use]
    pub fn rules_lines(&self, program: &str) -> Option<[String; 2]> {
        match self {
            Self::InvalidValue {
                key,
                value,
                expected,
            } if key.starts_with("rules.") => Some([
                format!("{program}: config.toml: {key} {value} is invalid"),
                expected.to_string(),
            ]),
            _ => None,
        }
    }
}

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
        Ok(config)
    }

    /// Returns a copy with the supplied command-line flag overrides applied.
    #[must_use]
    pub fn with_overrides(&self, overrides: &ConfigOverrides) -> Self {
        let mut config = self.clone();
        if let Some(model) = &overrides.model {
            config.model = Some(model.clone());
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
                        Box::from(name.as_ref()),
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

    fn builtins(product: ConfigProduct, data_root: &std::path::Path) -> Self {
        let (edit_style, guard, search_symbols) = match product {
            ConfigProduct::Dalgon => ("anchor", false, false),
            ConfigProduct::Dalgona => ("hashline", true, true),
        };
        Self {
            mode: Mode::Normal,
            model: None,
            thinking: crate::model::ThinkingLevel::Medium,
            approval: ApprovalMode::Ask,
            screen: Screen::Inline,
            theme: Box::from("auto"),
            sandbox: false,
            images: false,
            motion: true,
            compact_ratio: 0.85,
            edit_style: Box::from(edit_style),
            guard,
            search_symbols,
            judge: JudgeMode::Auto,
            plugins: Vec::new(),
            aliases: std::collections::BTreeMap::new(),
            serve: ServeConfig {
                bind: Box::from("127.0.0.1"),
                port: 7437,
                token_file: data_root.join("serve.token"),
                approval: ApprovalMode::Ask,
                origins: Vec::new(),
            },
            prices: std::collections::BTreeMap::new(),
            disabled_batteries: Vec::new(),
            experimental_batteries: Vec::new(),
            rules: RulesConfig::default(),
        }
    }

    fn apply_layer(
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
        if let Some(value) = layer.thinking.take() {
            self.thinking = value;
        }
        if let Some(value) = layer.approval.take() {
            self.approval = value;
        }
        if let Some(value) = layer.screen.take() {
            self.screen = value;
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
        if let Some(value) = layer.guard.take() {
            self.guard = value;
        }
        if let Some(value) = layer.search_symbols.take() {
            self.search_symbols = value;
        }
        if let Some(value) = layer.judge.take() {
            self.judge = value;
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
    }
}

fn replace_if_present<T>(target: &mut T, overlay: Option<T>) {
    if let Some(overlay) = overlay {
        *target = overlay;
    }
}

#[derive(Default)]
struct ParsedLayer {
    mode: Option<Mode>,
    model: Option<Box<str>>,
    thinking: Option<crate::model::ThinkingLevel>,
    approval: Option<ApprovalMode>,
    screen: Option<Screen>,
    theme: Option<Box<str>>,
    sandbox: Option<bool>,
    images: Option<bool>,
    motion: Option<bool>,
    compact_ratio: Option<f64>,
    edit_style: Option<Box<str>>,
    guard: Option<bool>,
    search_symbols: Option<bool>,
    judge: Option<JudgeMode>,
    plugins: Option<Vec<Box<str>>>,
    aliases: Option<std::collections::BTreeMap<Box<str>, Box<str>>>,
    serve: Option<PartialServe>,
    rules: Option<PartialRules>,
    prices: std::collections::BTreeMap<Box<str>, PartialPrice>,
    disabled_batteries: Option<Vec<Box<str>>>,
    experimental_batteries: Option<Vec<Box<str>>>,
}

#[derive(Default)]
struct PartialServe {
    bind: Option<Box<str>>,
    port: Option<u16>,
    token_file: Option<PathBuf>,
    approval: Option<ApprovalMode>,
    origins: Option<Vec<Box<str>>>,
}

#[derive(Default)]
struct PartialRules {
    watch: Option<bool>,
    interrupt: Option<crate::ext::InterruptMode>,
    repeat: Option<crate::ext::RepeatMode>,
    repeat_gap: Option<u16>,
    max_retries: Option<u32>,
    disabled: Option<Vec<Box<str>>>,
    judge: Option<JudgeMode>,
}

impl PartialRules {
    /// Replaces each leaf present in this layer; `disabled` replaces as a whole.
    fn apply_to(self, target: &mut RulesConfig) {
        replace_if_present(&mut target.watch, self.watch);
        replace_if_present(&mut target.interrupt, self.interrupt);
        replace_if_present(&mut target.repeat, self.repeat);
        replace_if_present(&mut target.repeat_gap, self.repeat_gap);
        replace_if_present(&mut target.max_retries, self.max_retries);
        replace_if_present(&mut target.disabled, self.disabled);
        replace_if_present(&mut target.judge, self.judge);
    }
}

#[derive(Default)]
struct PartialPrice {
    input: Option<f64>,
    cached_input: Option<f64>,
    output: Option<f64>,
    reasoning: Option<f64>,
}

impl PartialPrice {
    /// Folds one config layer into another: a rate present in `later` wins,
    /// including an explicit `0.0`; an absent rate keeps the earlier value.
    fn merge(&mut self, later: &Self) {
        self.input = later.input.or(self.input);
        self.cached_input = later.cached_input.or(self.cached_input);
        self.output = later.output.or(self.output);
        self.reasoning = later.reasoning.or(self.reasoning);
    }

    fn finish(self, model_id: &str) -> Result<crate::model::ModelPrice, ConfigError> {
        match (self.input, self.cached_input, self.output, self.reasoning) {
            (Some(input), Some(cached_input), Some(output), Some(reasoning)) => {
                Ok(crate::model::ModelPrice {
                    input,
                    cached_input,
                    output,
                    reasoning,
                })
            }
            (input, cached_input, output, reasoning) => {
                let missing = [
                    input.is_none().then_some("input"),
                    cached_input.is_none().then_some("cached_input"),
                    output.is_none().then_some("output"),
                    reasoning.is_none().then_some("reasoning"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                Err(invalid_value(
                    &format!("prices.{model_id}"),
                    format!("missing {}", missing.join(", ")),
                    "all four rates: input, cached_input, output, reasoning",
                ))
            }
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileText {
    mode: Option<toml::Value>,
    model: Option<toml::Value>,
    thinking: Option<toml::Value>,
    approval: Option<toml::Value>,
    screen: Option<toml::Value>,
    theme: Option<toml::Value>,
    sandbox: Option<toml::Value>,
    images: Option<toml::Value>,
    motion: Option<toml::Value>,
    compact_ratio: Option<toml::Value>,
    edit_style: Option<toml::Value>,
    guard: Option<toml::Value>,
    search_symbols: Option<toml::Value>,
    judge: Option<toml::Value>,
    plugins: Option<toml::Value>,
    aliases: Option<toml::Value>,
    serve: Option<toml::Value>,
    prices: Option<toml::Value>,
    rules: Option<toml::Value>,
    disabled_batteries: Option<toml::Value>,
    experimental_batteries: Option<toml::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ServeText {
    bind: Option<toml::Value>,
    port: Option<toml::Value>,
    token_file: Option<toml::Value>,
    approval: Option<toml::Value>,
    origins: Option<toml::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesText {
    watch: Option<toml::Value>,
    interrupt: Option<toml::Value>,
    repeat: Option<toml::Value>,
    repeat_gap: Option<toml::Value>,
    max_retries: Option<toml::Value>,
    disabled: Option<toml::Value>,
    judge: Option<toml::Value>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PriceText {
    input: Option<toml::Value>,
    cached_input: Option<toml::Value>,
    output: Option<toml::Value>,
    reasoning: Option<toml::Value>,
}

const DALGON_TOP_LEVEL_KEYS: &[&str] = &[
    "mode",
    "model",
    "thinking",
    "approval",
    "screen",
    "theme",
    "sandbox",
    "images",
    "motion",
    "compact_ratio",
    "edit_style",
    "guard",
    "search_symbols",
    "judge",
    "plugins",
    "aliases",
    "serve",
    "prices",
    "rules",
];

const DALGONA_TOP_LEVEL_KEYS: &[&str] = &[
    "mode",
    "model",
    "thinking",
    "approval",
    "screen",
    "theme",
    "sandbox",
    "images",
    "motion",
    "compact_ratio",
    "edit_style",
    "guard",
    "search_symbols",
    "judge",
    "plugins",
    "aliases",
    "serve",
    "prices",
    "rules",
    "disabled_batteries",
    "experimental_batteries",
];

const SERVE_KEYS: &[&str] = &["bind", "port", "token_file", "approval", "origins"];
const RULES_KEYS: &[&str] = &[
    "watch",
    "interrupt",
    "repeat",
    "repeat_gap",
    "max_retries",
    "disabled",
    "judge",
];
const PRICE_KEYS: &[&str] = &["input", "cached_input", "output", "reasoning"];

const KNOWN_KEY_ORDER: &[&str] = &[
    "mode",
    "model",
    "thinking",
    "approval",
    "screen",
    "theme",
    "sandbox",
    "images",
    "motion",
    "compact_ratio",
    "edit_style",
    "guard",
    "search_symbols",
    "judge",
    "plugins",
    "aliases",
    "serve",
    "serve.bind",
    "serve.port",
    "serve.token_file",
    "serve.approval",
    "serve.origins",
    "prices",
    "prices.<model-id>.input",
    "prices.<model-id>.cached_input",
    "prices.<model-id>.output",
    "prices.<model-id>.reasoning",
    "rules",
    "rules.watch",
    "rules.interrupt",
    "rules.repeat",
    "rules.repeat_gap",
    "rules.max_retries",
    "rules.disabled",
    "rules.judge",
    "disabled_batteries",
    "experimental_batteries",
];

fn parse_layer(product: ConfigProduct, text: &str) -> Result<ParsedLayer, ConfigError> {
    let value: toml::Value = toml::from_str(text).map_err(|error| syntax_error(text, &error))?;
    let root = match value {
        toml::Value::Table(root) => root,
        other => {
            return Err(invalid_value("", value_text(&other), "configuration table"));
        }
    };
    for key in root.keys() {
        if !top_level_keys(product).contains(&key.as_str()) {
            return Err(unknown_key(key, product, None));
        }
    }
    let file: FileText =
        toml::Value::Table(root)
            .try_into()
            .map_err(|error| ConfigError::Syntax {
                line: None,
                message: Box::from(error.message()),
            })?;
    parse_file_text(product, file)
}

fn top_level_keys(product: ConfigProduct) -> &'static [&'static str] {
    match product {
        ConfigProduct::Dalgon => DALGON_TOP_LEVEL_KEYS,
        ConfigProduct::Dalgona => DALGONA_TOP_LEVEL_KEYS,
    }
}

fn known_keys(product: ConfigProduct, price_model: Option<&str>) -> Vec<Box<str>> {
    KNOWN_KEY_ORDER
        .iter()
        .map(|key| {
            if let (Some(model), Some(rate)) = (price_model, key.strip_prefix("prices.<model-id>."))
            {
                format!("prices.{model}.{rate}").into_boxed_str()
            } else {
                Box::from(*key)
            }
        })
        .filter(|key| {
            product == ConfigProduct::Dalgona
                || (key.as_ref() != "disabled_batteries"
                    && key.as_ref() != "experimental_batteries")
        })
        .collect()
}

fn unknown_key(key: &str, product: ConfigProduct, price_model: Option<&str>) -> ConfigError {
    let known_keys = known_keys(product, price_model);
    let suggestion = closest_key(key, &known_keys);
    ConfigError::UnknownKey {
        key: Box::from(key),
        suggestion,
        known_keys,
    }
}

fn closest_key(key: &str, known_keys: &[Box<str>]) -> Option<Box<str>> {
    let mut closest: Option<(&Box<str>, usize)> = None;
    for candidate in known_keys {
        let distance = edit_distance(key.as_bytes(), candidate.as_bytes());
        if distance > 2 {
            continue;
        }
        match closest {
            Some((_, best_distance)) if distance >= best_distance => {}
            _ => closest = Some((candidate, distance)),
        }
    }
    closest.map(|(candidate, _)| Box::from(candidate.as_ref()))
}

fn edit_distance(left: &[u8], right: &[u8]) -> usize {
    const MAX_DISTANCE: usize = 2;
    if left.len().abs_diff(right.len()) > MAX_DISTANCE {
        return MAX_DISTANCE + 1;
    }
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_byte) in left.iter().enumerate() {
        current[0] = left_index + 1;
        let mut row_min = current[0];
        for (right_index, right_byte) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_byte != right_byte);
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(substitution);
            row_min = row_min.min(current[right_index + 1]);
        }
        if row_min > MAX_DISTANCE {
            return MAX_DISTANCE + 1;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn syntax_error(text: &str, error: &toml::de::Error) -> ConfigError {
    let line = error.span().and_then(|span| {
        text.get(..span.start)
            .map(|prefix| prefix.bytes().filter(|byte| *byte == b'\n').count() + 1)
    });
    ConfigError::Syntax {
        line,
        message: Box::from(error.message()),
    }
}

fn invalid_value(key: &str, value: impl Into<Box<str>>, expected: &str) -> ConfigError {
    ConfigError::InvalidValue {
        key: Box::from(key),
        value: value.into(),
        expected: Box::from(expected),
    }
}

fn value_text(value: &toml::Value) -> Box<str> {
    match value {
        toml::Value::String(value) => Box::from(value.as_str()),
        _ => format!("{value}").into_boxed_str(),
    }
}

fn parse_file_text(product: ConfigProduct, file: FileText) -> Result<ParsedLayer, ConfigError> {
    let mut layer = ParsedLayer::default();
    if let Some(value) = file.mode {
        layer.mode = Some(parse_mode(value)?);
    }
    if let Some(value) = file.model {
        layer.model = Some(parse_nonempty_string("model", value, "non-empty model id")?);
    }
    if let Some(value) = file.thinking {
        layer.thinking = Some(parse_thinking(value)?);
    }
    if let Some(value) = file.approval {
        layer.approval = Some(parse_approval("approval", value)?);
    }
    if let Some(value) = file.screen {
        layer.screen = Some(parse_screen(value)?);
    }
    if let Some(value) = file.theme {
        layer.theme = Some(parse_nonempty_string("theme", value, "non-empty string")?);
    }
    if let Some(value) = file.sandbox {
        layer.sandbox = Some(parse_bool("sandbox", value)?);
    }
    if let Some(value) = file.images {
        layer.images = Some(parse_bool("images", value)?);
    }
    if let Some(value) = file.motion {
        layer.motion = Some(parse_bool("motion", value)?);
    }
    if let Some(value) = file.compact_ratio {
        let ratio = parse_number("compact_ratio", value, "finite number in 0.5..=0.95")?;
        if !ratio.is_finite() || !(0.5..=0.95).contains(&ratio) {
            return Err(invalid_value(
                "compact_ratio",
                format!("{ratio}"),
                "finite number in 0.5..=0.95",
            ));
        }
        layer.compact_ratio = Some(ratio);
    }
    if let Some(value) = file.edit_style {
        layer.edit_style = Some(parse_edit_style(value)?);
    }
    if let Some(value) = file.guard {
        layer.guard = Some(parse_bool("guard", value)?);
    }
    if let Some(value) = file.search_symbols {
        layer.search_symbols = Some(parse_bool("search_symbols", value)?);
    }
    if let Some(value) = file.judge {
        layer.judge = Some(parse_judge(value)?);
    }
    if let Some(value) = file.plugins {
        layer.plugins = Some(parse_nonempty_strings(
            "plugins",
            value,
            "array of non-empty strings",
        )?);
    }
    if let Some(value) = file.aliases {
        layer.aliases = Some(parse_aliases(value)?);
    }
    if let Some(value) = file.serve {
        layer.serve = Some(parse_serve(product, value)?);
    }
    if let Some(value) = file.rules {
        layer.rules = Some(parse_rules(product, value)?);
    }
    if let Some(value) = file.prices {
        layer.prices = parse_prices(product, value)?;
    }
    if let Some(value) = file.disabled_batteries {
        layer.disabled_batteries = Some(parse_nonempty_strings(
            "disabled_batteries",
            value,
            "array of non-empty battery names",
        )?);
    }
    if let Some(value) = file.experimental_batteries {
        layer.experimental_batteries = Some(parse_nonempty_strings(
            "experimental_batteries",
            value,
            "array of non-empty battery names",
        )?);
    }
    Ok(layer)
}

fn parse_mode(value: toml::Value) -> Result<Mode, ConfigError> {
    const EXPECTED: &str = "normal, eval-first, eval-only";
    match value {
        toml::Value::String(value) => match value.as_str() {
            "normal" => Ok(Mode::Normal),
            "eval-first" => Ok(Mode::EvalFirst),
            "eval-only" => Ok(Mode::EvalOnly),
            _ => Err(invalid_value("mode", value.into_boxed_str(), EXPECTED)),
        },
        value => Err(invalid_value("mode", value_text(&value), EXPECTED)),
    }
}

fn parse_thinking(value: toml::Value) -> Result<crate::model::ThinkingLevel, ConfigError> {
    use crate::model::ThinkingLevel;
    const EXPECTED: &str = "off, minimal, low, medium, high, xhigh, max";
    match value {
        toml::Value::String(value) => match value.as_str() {
            "off" => Ok(ThinkingLevel::Off),
            "minimal" => Ok(ThinkingLevel::Minimal),
            "low" => Ok(ThinkingLevel::Low),
            "medium" => Ok(ThinkingLevel::Medium),
            "high" => Ok(ThinkingLevel::High),
            "xhigh" => Ok(ThinkingLevel::Xhigh),
            "max" => Ok(ThinkingLevel::Max),
            _ => Err(invalid_value("thinking", value.into_boxed_str(), EXPECTED)),
        },
        value => Err(invalid_value("thinking", value_text(&value), EXPECTED)),
    }
}

fn parse_approval(key: &str, value: toml::Value) -> Result<ApprovalMode, ConfigError> {
    const EXPECTED: &str = "ask, edits, all";
    match value {
        toml::Value::String(value) => match value.as_str() {
            "ask" => Ok(ApprovalMode::Ask),
            "edits" => Ok(ApprovalMode::Edits),
            "all" => Ok(ApprovalMode::All),
            _ => Err(invalid_value(key, value.into_boxed_str(), EXPECTED)),
        },
        value => Err(invalid_value(key, value_text(&value), EXPECTED)),
    }
}

fn parse_screen(value: toml::Value) -> Result<Screen, ConfigError> {
    const EXPECTED: &str = "inline, fullscreen";
    match value {
        toml::Value::String(value) => match value.as_str() {
            "inline" => Ok(Screen::Inline),
            "fullscreen" => Ok(Screen::Fullscreen),
            _ => Err(invalid_value("screen", value.into_boxed_str(), EXPECTED)),
        },
        value => Err(invalid_value("screen", value_text(&value), EXPECTED)),
    }
}

fn parse_judge(value: toml::Value) -> Result<JudgeMode, ConfigError> {
    const EXPECTED: &str = "auto, on, off";
    match value {
        toml::Value::String(value) => match value.as_str() {
            "auto" => Ok(JudgeMode::Auto),
            "on" => Ok(JudgeMode::On),
            "off" => Ok(JudgeMode::Off),
            _ => Err(invalid_value("judge", value.into_boxed_str(), EXPECTED)),
        },
        value => Err(invalid_value("judge", value_text(&value), EXPECTED)),
    }
}

fn parse_edit_style(value: toml::Value) -> Result<Box<str>, ConfigError> {
    const EXPECTED: &str = "replace, apply_patch, anchor, hashline, simple, balanced, strict";
    match value {
        toml::Value::String(value) => {
            let alias = match value.as_str() {
                "simple" => Some("replace"),
                "balanced" => Some("anchor"),
                "strict" => Some("hashline"),
                "replace" | "apply_patch" | "anchor" | "hashline" => None,
                _ => {
                    return Err(invalid_value(
                        "edit_style",
                        Box::from(value.as_str()),
                        EXPECTED,
                    ));
                }
            };
            Ok(alias.map_or_else(|| value.into_boxed_str(), Box::from))
        }
        value => Err(invalid_value("edit_style", value_text(&value), EXPECTED)),
    }
}

fn parse_nonempty_string(
    key: &str,
    value: toml::Value,
    expected: &str,
) -> Result<Box<str>, ConfigError> {
    match value {
        toml::Value::String(value) if !value.is_empty() => Ok(value.into_boxed_str()),
        toml::Value::String(value) => Err(invalid_value(key, value.into_boxed_str(), expected)),
        value => Err(invalid_value(key, value_text(&value), expected)),
    }
}

fn parse_bool(key: &str, value: toml::Value) -> Result<bool, ConfigError> {
    match value {
        toml::Value::Boolean(value) => Ok(value),
        value => Err(invalid_value(key, value_text(&value), "boolean")),
    }
}

fn parse_port(value: toml::Value) -> Result<u16, ConfigError> {
    match value {
        toml::Value::Integer(value) => u16::try_from(value)
            .map_err(|_| invalid_value("serve.port", format!("{value}"), "integer 0..=65535")),
        value => Err(invalid_value(
            "serve.port",
            value_text(&value),
            "integer 0..=65535",
        )),
    }
}

fn parse_number(key: &str, value: toml::Value, expected: &str) -> Result<f64, ConfigError> {
    match value {
        toml::Value::Integer(value) => toml::Value::Integer(value)
            .try_into()
            .map_err(|_| invalid_value(key, format!("{value}"), expected)),
        toml::Value::Float(value) => Ok(value),
        value => Err(invalid_value(key, value_text(&value), expected)),
    }
}

fn parse_strings(
    key: &str,
    value: toml::Value,
    expected: &str,
) -> Result<Vec<Box<str>>, ConfigError> {
    let values = match value {
        toml::Value::Array(values) => values,
        other => return Err(invalid_value(key, value_text(&other), expected)),
    };
    values
        .into_iter()
        .map(|value| match value {
            toml::Value::String(value) => Ok(value.into_boxed_str()),
            value => Err(invalid_value(key, value_text(&value), expected)),
        })
        .collect()
}

fn parse_nonempty_strings(
    key: &str,
    value: toml::Value,
    expected: &str,
) -> Result<Vec<Box<str>>, ConfigError> {
    let values = parse_strings(key, value, expected)?;
    if let Some(empty) = values.iter().find(|value| value.is_empty()) {
        return Err(invalid_value(key, Box::from(empty.as_ref()), expected));
    }
    Ok(values)
}

fn parse_aliases(
    value: toml::Value,
) -> Result<std::collections::BTreeMap<Box<str>, Box<str>>, ConfigError> {
    let aliases = match value {
        toml::Value::Table(aliases) => aliases,
        other => {
            return Err(invalid_value(
                "aliases",
                value_text(&other),
                "table of model ids",
            ));
        }
    };
    let mut parsed = std::collections::BTreeMap::new();
    for (alias, value) in aliases {
        let key = format!("aliases.{alias}");
        let model = parse_nonempty_string(&key, value, "non-empty model id")?;
        parsed.insert(alias.into_boxed_str(), model);
    }
    Ok(parsed)
}

fn parse_path(value: toml::Value) -> Result<PathBuf, ConfigError> {
    match value {
        toml::Value::String(value) => Ok(PathBuf::from(value)),
        value => Err(invalid_value(
            "serve.token_file",
            value_text(&value),
            "path string",
        )),
    }
}

fn parse_serve(product: ConfigProduct, value: toml::Value) -> Result<PartialServe, ConfigError> {
    let table = match value {
        toml::Value::Table(table) => table,
        other => return Err(invalid_value("serve", value_text(&other), "table")),
    };
    for key in table.keys() {
        if !SERVE_KEYS.contains(&key.as_str()) {
            return Err(unknown_key(&format!("serve.{key}"), product, None));
        }
    }
    let text: ServeText =
        toml::Value::Table(table)
            .try_into()
            .map_err(|error| ConfigError::Syntax {
                line: None,
                message: Box::from(error.message()),
            })?;
    let mut serve = PartialServe::default();
    if let Some(value) = text.bind {
        serve.bind = Some(parse_nonempty_string(
            "serve.bind",
            value,
            "non-empty address string",
        )?);
    }
    if let Some(value) = text.port {
        serve.port = Some(parse_port(value)?);
    }
    if let Some(value) = text.token_file {
        serve.token_file = Some(parse_path(value)?);
    }
    if let Some(value) = text.approval {
        serve.approval = Some(parse_approval("serve.approval", value)?);
    }
    if let Some(value) = text.origins {
        serve.origins = Some(parse_strings(
            "serve.origins",
            value,
            "array of exact origin strings",
        )?);
    }
    Ok(serve)
}

fn parse_rules(product: ConfigProduct, value: toml::Value) -> Result<PartialRules, ConfigError> {
    use crate::ext::{InterruptMode, RepeatMode};

    let table = match value {
        toml::Value::Table(table) => table,
        other => return Err(invalid_value("rules", value_text(&other), "table")),
    };
    for key in table.keys() {
        if !RULES_KEYS.contains(&key.as_str()) {
            return Err(unknown_key(&format!("rules.{key}"), product, None));
        }
    }
    let text: RulesText =
        toml::Value::Table(table)
            .try_into()
            .map_err(|error| ConfigError::Syntax {
                line: None,
                message: Box::from(error.message()),
            })?;
    let mut rules = PartialRules::default();
    if let Some(value) = text.watch {
        rules.watch = Some(match value {
            toml::Value::Boolean(watch) => watch,
            other => return Err(rules_invalid("rules.watch", &other, "Use true or false.")),
        });
    }
    if let Some(value) = text.interrupt {
        rules.interrupt = Some(parse_rules_choice(
            "rules.interrupt",
            &value,
            &[
                ("always", InterruptMode::Always),
                ("prose-only", InterruptMode::ProseOnly),
                ("tool-only", InterruptMode::ToolOnly),
                ("never", InterruptMode::Never),
            ],
            "Use one of always, prose-only, tool-only, never.",
        )?);
    }
    if let Some(value) = text.repeat {
        rules.repeat = Some(parse_rules_choice(
            "rules.repeat",
            &value,
            &[
                ("once", RepeatMode::Once),
                ("after-gap", RepeatMode::AfterGap),
            ],
            "Use one of once, after-gap.",
        )?);
    }
    if let Some(value) = text.repeat_gap {
        rules.repeat_gap = Some(parse_rules_integer(
            "rules.repeat_gap",
            &value,
            1..=1000,
            "Use a whole number from 1 to 1000.",
        )?);
    }
    if let Some(value) = text.max_retries {
        rules.max_retries = Some(parse_rules_integer(
            "rules.max_retries",
            &value,
            0..=20,
            "Use a whole number from 0 to 20.",
        )?);
    }
    if let Some(value) = text.disabled {
        rules.disabled = Some(parse_rules_disabled(&value)?);
    }
    if let Some(value) = text.judge {
        rules.judge = Some(parse_rules_choice(
            "rules.judge",
            &value,
            &[
                ("auto", JudgeMode::Auto),
                ("on", JudgeMode::On),
                ("off", JudgeMode::Off),
            ],
            "Use one of auto, on, off.",
        )?);
    }
    Ok(rules)
}

/// Builds a `[rules]` value error: the value keeps its TOML form and
/// `expected` holds the exact second diagnostic line.
fn rules_invalid(key: &str, value: &toml::Value, hint: &str) -> ConfigError {
    invalid_value(key, value.to_string(), hint)
}

fn parse_rules_choice<T: Copy>(
    key: &str,
    value: &toml::Value,
    choices: &[(&str, T)],
    hint: &str,
) -> Result<T, ConfigError> {
    if let toml::Value::String(text) = value
        && let Some((_, choice)) = choices
            .iter()
            .find(|(spelling, _)| *spelling == text.as_str())
    {
        return Ok(*choice);
    }
    Err(rules_invalid(key, value, hint))
}

fn parse_rules_integer<T: TryFrom<i64>>(
    key: &str,
    value: &toml::Value,
    range: std::ops::RangeInclusive<i64>,
    hint: &str,
) -> Result<T, ConfigError> {
    if let toml::Value::Integer(number) = *value
        && range.contains(&number)
        && let Ok(number) = T::try_from(number)
    {
        return Ok(number);
    }
    Err(rules_invalid(key, value, hint))
}

fn parse_rules_disabled(value: &toml::Value) -> Result<Vec<Box<str>>, ConfigError> {
    if let toml::Value::Array(items) = value
        && let Some(names) = items
            .iter()
            .map(|item| match item {
                toml::Value::String(name) if is_rule_name(name) => Some(Box::from(name.as_str())),
                _ => None,
            })
            .collect::<Option<Vec<Box<str>>>>()
    {
        return Ok(names);
    }
    Err(rules_invalid(
        "rules.disabled",
        value,
        "Use a list of rule names, such as [\"no-sleep\"].",
    ))
}

/// Checks the rule name grammar `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
fn is_rule_name(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.len() <= 64
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'-'))
}

fn parse_prices(
    product: ConfigProduct,
    value: toml::Value,
) -> Result<std::collections::BTreeMap<Box<str>, PartialPrice>, ConfigError> {
    let prices = match value {
        toml::Value::Table(prices) => prices,
        other => {
            return Err(invalid_value(
                "prices",
                value_text(&other),
                "table of model price tables",
            ));
        }
    };
    let mut parsed = std::collections::BTreeMap::new();
    for (model_id, value) in prices {
        if model_id.is_empty() {
            return Err(invalid_value("prices", Box::from(""), "non-empty model id"));
        }
        let rates = match value {
            toml::Value::Table(rates) => rates,
            other => {
                return Err(invalid_value(
                    &format!("prices.{model_id}"),
                    value_text(&other),
                    "table of price rates",
                ));
            }
        };
        for key in rates.keys() {
            if !PRICE_KEYS.contains(&key.as_str()) {
                return Err(unknown_key(
                    &format!("prices.{model_id}.{key}"),
                    product,
                    Some(&model_id),
                ));
            }
        }
        let text: PriceText =
            toml::Value::Table(rates)
                .try_into()
                .map_err(|error| ConfigError::Syntax {
                    line: None,
                    message: Box::from(error.message()),
                })?;
        let mut partial = PartialPrice::default();
        if let Some(value) = text.input {
            partial.input = Some(parse_rate(&format!("prices.{model_id}.input"), value)?);
        }
        if let Some(value) = text.cached_input {
            partial.cached_input = Some(parse_rate(
                &format!("prices.{model_id}.cached_input"),
                value,
            )?);
        }
        if let Some(value) = text.output {
            partial.output = Some(parse_rate(&format!("prices.{model_id}.output"), value)?);
        }
        if let Some(value) = text.reasoning {
            partial.reasoning = Some(parse_rate(&format!("prices.{model_id}.reasoning"), value)?);
        }
        parsed.insert(model_id.into_boxed_str(), partial);
    }
    Ok(parsed)
}

fn parse_rate(key: &str, value: toml::Value) -> Result<f64, ConfigError> {
    let rate = parse_number(key, value, "finite non-negative USD rate")?;
    if !rate.is_finite() || rate < 0.0 {
        return Err(invalid_value(
            key,
            format!("{rate}"),
            "finite non-negative USD rate",
        ));
    }
    Ok(rate)
}

fn finish_prices(
    price_layers: std::collections::BTreeMap<Box<str>, PartialPrice>,
) -> Result<std::collections::BTreeMap<Box<str>, crate::model::ModelPrice>, ConfigError> {
    let mut prices = std::collections::BTreeMap::new();
    for (model_id, price) in price_layers {
        let price = price.finish(&model_id)?;
        prices.insert(model_id, price);
    }
    Ok(prices)
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalMode, Config, ConfigError, ConfigOverrides, ConfigProduct, JudgeMode, Mode,
        ModeProjection, Screen,
    };
    use crate::model::{ModelPrice, ThinkingLevel};
    use std::path::{Path, PathBuf};

    const DATA_ROOT: &str = "/home/test/dalgon";
    const DALGONA_DEFAULTS: &str = r#"
mode = "eval-first"
model = "base/model"
thinking = "low"
approval = "edits"
screen = "fullscreen"
theme = "dusk"
sandbox = true
images = true
motion = false
compact_ratio = 0.75
edit_style = "balanced"
guard = false
search_symbols = false
judge = "off"
plugins = ["first", "second"]
disabled_batteries = ["ask", "search"]
experimental_batteries = ["skills"]

[aliases]
fast = "family/base"
slow = "dalgon/normal"

[serve]
bind = "127.0.0.2"
port = 9000
token_file = "state/token"
origins = ["https://default.example"]

[prices."family/model"]
input = 1.0
cached_input = 0.5
output = 2.0
reasoning = 3.0
"#;

    fn load(product: ConfigProduct, user_toml: &str) -> Result<Config, ConfigError> {
        Config::load(product, Path::new(DATA_ROOT), "", Some(user_toml))
    }

    fn parse_enum<T: serde::de::DeserializeOwned>(spelling: &str) -> Result<T, toml::de::Error> {
        let document = format!("value = {spelling:?}");
        let mut table: toml::Table = toml::from_str(&document)?;
        table
            .remove("value")
            .expect("the test document contains value")
            .try_into()
    }

    #[test]
    fn mode_spellings_are_closed_and_canonical() {
        for (spelling, mode) in [
            ("normal", Mode::Normal),
            ("eval-first", Mode::EvalFirst),
            ("eval-only", Mode::EvalOnly),
        ] {
            assert_eq!(parse_enum::<Mode>(spelling).unwrap(), mode);
            assert_eq!(mode.as_str(), spelling);
        }
        for spelling in ["NORMAL", "eval_first", "evalfirst", "other"] {
            assert!(parse_enum::<Mode>(spelling).is_err(), "accepted {spelling}");
        }
    }

    #[test]
    fn other_policy_spellings_are_closed_snake_case() {
        for spelling in ["ask", "edits", "all"] {
            assert!(parse_enum::<ApprovalMode>(spelling).is_ok());
        }
        for spelling in ["inline", "fullscreen"] {
            assert!(parse_enum::<Screen>(spelling).is_ok());
        }
        for spelling in ["auto", "on", "off"] {
            assert!(parse_enum::<JudgeMode>(spelling).is_ok());
        }
        for spelling in ["ASK", "eval-first", "full_screen", "enabled"] {
            assert!(parse_enum::<ApprovalMode>(spelling).is_err());
            assert!(parse_enum::<Screen>(spelling).is_err());
            assert!(parse_enum::<JudgeMode>(spelling).is_err());
        }
    }

    #[test]
    fn mode_projection_matches_all_modes() {
        assert_eq!(
            Mode::Normal.projection(),
            ModeProjection {
                eval_first: false,
                non_eval_only: false,
            }
        );
        assert_eq!(
            Mode::EvalFirst.projection(),
            ModeProjection {
                eval_first: true,
                non_eval_only: false,
            }
        );
        assert_eq!(
            Mode::EvalOnly.projection(),
            ModeProjection {
                eval_first: false,
                non_eval_only: true,
            }
        );
    }

    #[test]
    fn config_layers_defaults_and_replaces_whole_values() {
        let user = r#"
mode = "eval-only"
model = "user/model"
plugins = ["user"]
disabled_batteries = ["web"]
experimental_batteries = []

[aliases]
fast = "family/updated"

[serve]
origins = ["https://user.example"]

[prices."family/model"]
cached_input = 0.0
output = 4.0
reasoning = 5.0
"#;
        let config = Config::load(
            ConfigProduct::Dalgona,
            Path::new(DATA_ROOT),
            DALGONA_DEFAULTS,
            Some(user),
        )
        .expect("valid product and user layers");
        assert_eq!(config.mode(), Mode::EvalOnly);
        assert_eq!(config.model(), Some("user/model"));
        assert_eq!(config.thinking(), ThinkingLevel::Low);
        assert_eq!(config.approval(), ApprovalMode::Edits);
        assert_eq!(config.screen(), Screen::Fullscreen);
        assert_eq!(config.theme.as_ref(), "dusk");
        assert!(config.sandbox);
        assert!(config.images);
        assert!(!config.motion);
        assert_eq!(config.compact_ratio.to_bits(), 0.75_f64.to_bits());
        assert_eq!(config.edit_style.as_ref(), "anchor");
        assert!(!config.guard);
        assert!(!config.search_symbols);
        assert_eq!(config.judge, JudgeMode::Off);
        assert_eq!(config.plugins, vec![Box::from("user")]);
        assert_eq!(config.aliases.len(), 1);
        assert_eq!(
            config.aliases.get("fast").map(AsRef::as_ref),
            Some("family/updated")
        );
        assert!(!config.aliases.contains_key("slow"));
        assert_eq!(config.disabled_batteries, vec![Box::from("web")]);
        assert!(config.experimental_batteries.is_empty());
        assert_eq!(
            config.serve().origins,
            vec![Box::from("https://user.example")]
        );
        assert_eq!(config.serve().approval, ApprovalMode::Edits);
        assert_eq!(
            config.price_for_model("family/model"),
            Some(&ModelPrice {
                input: 1.0,
                cached_input: 0.0,
                output: 4.0,
                reasoning: 5.0,
            })
        );

        let without_user = Config::load(
            ConfigProduct::Dalgona,
            Path::new(DATA_ROOT),
            DALGONA_DEFAULTS,
            None,
        )
        .expect("valid defaults without user text");
        let with_empty_user = Config::load(
            ConfigProduct::Dalgona,
            Path::new(DATA_ROOT),
            DALGONA_DEFAULTS,
            Some(""),
        )
        .expect("empty user text is valid");
        assert_eq!(without_user, with_empty_user);
    }

    #[test]
    fn config_rejects_unknown_and_invalid_values() {
        let error = load(ConfigProduct::Dalgon, "modle = 1").expect_err("unknown key");
        assert!(matches!(
            error,
            ConfigError::UnknownKey { key, suggestion, .. }
                if key.as_ref() == "modle" && suggestion.as_deref() == Some("mode")
        ));

        let error = load(ConfigProduct::Dalgon, "zzz = 1").expect_err("distant unknown key");
        assert!(matches!(
            error,
            ConfigError::UnknownKey {
                suggestion: None,
                ..
            }
        ));

        let error =
            load(ConfigProduct::Dalgon, "mode = \"jit\"").expect_err("unsupported execution mode");
        assert!(matches!(
            error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "mode"
                    && value.as_ref() == "jit"
                    && expected.as_ref() == "normal, eval-first, eval-only"
        ));

        let error = load(
            ConfigProduct::Dalgon,
            "mode = \"normal\"\nmode = \"eval-first\"",
        )
        .expect_err("duplicate TOML keys");
        assert!(matches!(error, ConfigError::Syntax { line: Some(2), .. }));

        let error = load(ConfigProduct::Dalgon, "[serve]\ntoken = \"secret\"")
            .expect_err("serve.token is not a key");
        assert!(matches!(
            error,
            ConfigError::UnknownKey { key, .. } if key.as_ref() == "serve.token"
        ));

        let error = load(ConfigProduct::Dalgon, "[prices.model]\ninpt = 1.0")
            .expect_err("price rate keys are closed");
        assert!(matches!(
            error,
            ConfigError::UnknownKey { key, suggestion, .. }
                if key.as_ref() == "prices.model.inpt"
                    && suggestion.as_deref() == Some("prices.model.input")
        ));
    }

    #[test]
    fn config_closes_product_keys_and_checks_batteries_after_registration() {
        let error = load(ConfigProduct::Dalgon, "disabled_batteries = []")
            .expect_err("dalgon does not support battery keys");
        assert!(matches!(
            error,
            ConfigError::UnknownKey { key, .. }
                if key.as_ref() == "disabled_batteries"
        ));

        let empty = load(
            ConfigProduct::Dalgona,
            "disabled_batteries = []\nexperimental_batteries = []",
        )
        .expect("empty battery lists are valid");
        assert!(empty.validate_battery_names(&[]).is_ok());

        let error = load(ConfigProduct::Dalgona, "disabled_batteries = [\"\"]")
            .expect_err("empty battery names are invalid at parse time");
        assert!(matches!(error, ConfigError::InvalidValue { .. }));

        let config = load(
            ConfigProduct::Dalgona,
            "disabled_batteries = [\"ask\"]\nexperimental_batteries = [\"missing\"]",
        )
        .expect("non-empty battery names parse before registration");
        let error = config
            .validate_battery_names(&["ask", "search"])
            .expect_err("unknown battery names fail after registration");
        assert!(matches!(
            error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "experimental_batteries"
                    && value.as_ref() == "missing"
                    && expected.as_ref() == "registered battery names: ask, search"
        ));

        let config = load(ConfigProduct::Dalgona, "disabled_batteries = [\"missing\"]")
            .expect("unknown names are deferred until registration");
        assert!(matches!(
            config.validate_battery_names(&["ask"]),
            Err(ConfigError::InvalidValue { key, .. })
                if key.as_ref() == "disabled_batteries"
        ));
    }

    #[test]
    fn config_prices_and_serve_origins_are_typed() {
        let config = load(
            ConfigProduct::Dalgon,
            r#"
approval = "edits"

[serve]
token_file = ""
origins = ["https://one.example", "https://two.example"]

[prices."family/model"]
input = 1.0
cached_input = 0.5
output = 2.0
reasoning = 3.0
"#,
        )
        .expect("valid serve and price values");
        assert_eq!(config.serve().approval, ApprovalMode::Edits);
        assert_eq!(
            config.serve().origins,
            vec![
                Box::from("https://one.example"),
                Box::from("https://two.example")
            ]
        );
        assert_eq!(config.serve().token_file, PathBuf::new());
        assert_eq!(
            config.price_for_model("family/model"),
            Some(&ModelPrice {
                input: 1.0,
                cached_input: 0.5,
                output: 2.0,
                reasoning: 3.0,
            })
        );

        let defaults = load(ConfigProduct::Dalgon, "").expect("built-in defaults");
        assert!(defaults.serve().origins.is_empty());
        assert_eq!(defaults.serve().approval, ApprovalMode::Ask);
        assert_eq!(
            defaults.serve().token_file,
            PathBuf::from(DATA_ROOT).join("serve.token")
        );

        for port in ["0", "65535"] {
            let document = format!("[serve]\nport = {port}");
            assert!(load(ConfigProduct::Dalgon, &document).is_ok());
        }
        for port in ["-1", "65536"] {
            let document = format!("[serve]\nport = {port}");
            assert!(matches!(
                load(ConfigProduct::Dalgon, &document),
                Err(ConfigError::InvalidValue { key, .. })
                    if key.as_ref() == "serve.port"
            ));
        }

        for rate in ["-1.0", "nan", "inf"] {
            let document = format!(
                "[prices.model]\ninput = {rate}\ncached_input = 0.0\noutput = 0.0\nreasoning = 0.0"
            );
            assert!(matches!(
                load(ConfigProduct::Dalgon, &document),
                Err(ConfigError::InvalidValue { key, expected, .. })
                    if key.as_ref() == "prices.model.input"
                        && expected.as_ref() == "finite non-negative USD rate"
            ));
        }

        let zero = load(
            ConfigProduct::Dalgon,
            "[prices.zero]\ninput = 0.0\ncached_input = 0.0\noutput = 0.0\nreasoning = 0.0",
        )
        .expect("explicit zero prices are valid");
        assert_eq!(
            zero.price_for_model("zero"),
            Some(&ModelPrice {
                input: 0.0,
                cached_input: 0.0,
                output: 0.0,
                reasoning: 0.0,
            })
        );
    }

    #[test]
    fn config_merges_price_fields_and_rejects_incomplete_rates() {
        let config = Config::load(
            ConfigProduct::Dalgon,
            Path::new(DATA_ROOT),
            "[prices.partial]\ninput = 1.0\ncached_input = 2.0",
            Some("[prices.partial]\noutput = 3.0\nreasoning = 4.0"),
        )
        .expect("rate fields merge across layers");
        assert_eq!(
            config.price_for_model("partial"),
            Some(&ModelPrice {
                input: 1.0,
                cached_input: 2.0,
                output: 3.0,
                reasoning: 4.0,
            })
        );

        let error = load(ConfigProduct::Dalgon, "[prices.partial]\ninput = 0.0")
            .expect_err("missing rates are not zero");
        assert!(matches!(
            error,
            ConfigError::InvalidValue { key, value, expected }
                if key.as_ref() == "prices.partial"
                    && value.as_ref() == "missing cached_input, output, reasoning"
                    && expected.as_ref() == "all four rates: input, cached_input, output, reasoning"
        ));
    }

    #[test]
    fn config_price_layers_override_single_rates_including_zero() {
        let config = Config::load(
            ConfigProduct::Dalgon,
            Path::new(DATA_ROOT),
            "[prices.partial]\ninput = 1.0\ncached_input = 2.0\noutput = 3.0\nreasoning = 4.0",
            Some("[prices.partial]\ncached_input = 0.0\nreasoning = 5.0"),
        )
        .expect("later rates replace earlier ones");
        assert_eq!(
            config.price_for_model("partial"),
            Some(&ModelPrice {
                input: 1.0,
                cached_input: 0.0,
                output: 3.0,
                reasoning: 5.0,
            })
        );
    }

    #[test]
    fn config_rejects_corrupt_compiled_defaults_before_user_text() {
        let error = Config::load(
            ConfigProduct::Dalgon,
            Path::new(DATA_ROOT),
            "mode = [",
            Some("mode = ["),
        )
        .expect_err("compiled default corruption is internal");
        assert!(matches!(
            error,
            ConfigError::DefaultsInvalid { message }
                if message.contains("Syntax")
        ));
    }

    #[test]
    fn config_flag_overrides_are_in_memory() {
        let config = load(
            ConfigProduct::Dalgon,
            "model = \"base/model\"\nmode = \"eval-first\"\napproval = \"edits\"\nscreen = \"inline\"\nsandbox = false",
        )
        .expect("valid base configuration");
        let overrides = ConfigOverrides {
            model: Some(Box::from("flag/model")),
            mode: Some(Mode::EvalOnly),
            thinking: Some(ThinkingLevel::High),
            approval: Some(ApprovalMode::All),
            screen: Some(Screen::Fullscreen),
            sandbox: Some(true),
        };
        let overlaid = config.with_overrides(&overrides);
        assert_eq!(overlaid.model(), Some("flag/model"));
        assert_eq!(overlaid.mode(), Mode::EvalOnly);
        assert_eq!(overlaid.thinking(), ThinkingLevel::High);
        assert_eq!(overlaid.approval(), ApprovalMode::All);
        assert_eq!(overlaid.screen(), Screen::Fullscreen);
        assert!(overlaid.sandbox);
        assert_eq!(overlaid.serve().approval, ApprovalMode::Edits);
        assert_eq!(config.model(), Some("base/model"));
        assert_eq!(config.approval(), ApprovalMode::Edits);
        assert_eq!(config.serve().approval, ApprovalMode::Edits);

        assert_eq!(config.with_overrides(&ConfigOverrides::default()), config);
    }

    #[test]
    fn config_resolves_edit_style_tiers() {
        for (value, expected) in [
            ("simple", "replace"),
            ("replace", "replace"),
            ("balanced", "anchor"),
            ("anchor", "anchor"),
            ("strict", "hashline"),
            ("hashline", "hashline"),
            ("apply_patch", "apply_patch"),
        ] {
            let document = format!("edit_style = {value:?}");
            let config =
                load(ConfigProduct::Dalgon, &document).expect("recognized edit style or tier");
            assert_eq!(config.edit_style.as_ref(), expected);
        }
    }

    #[test]
    fn compact_ratio_enforces_finite_inclusive_bounds() {
        for ratio in ["0.5", "0.95"] {
            let document = format!("compact_ratio = {ratio}");
            assert!(load(ConfigProduct::Dalgon, &document).is_ok());
        }
        for ratio in ["0.49", "0.96", "nan", "inf"] {
            let document = format!("compact_ratio = {ratio}");
            assert!(matches!(
                load(ConfigProduct::Dalgon, &document),
                Err(ConfigError::InvalidValue { key, .. })
                    if key.as_ref() == "compact_ratio"
            ));
        }
    }

    fn rules_error_lines(product: ConfigProduct, rules_body: &str) -> [String; 2] {
        let program = match product {
            ConfigProduct::Dalgon => "dalgon",
            ConfigProduct::Dalgona => "dalgona",
        };
        load(product, &format!("[rules]\n{rules_body}"))
            .expect_err("the rules value is invalid")
            .rules_lines(program)
            .expect("a rules value error has two lines")
    }

    #[test]
    fn rules_defaults_and_layer_precedence() {
        use super::RulesConfig;
        use crate::ext::{InterruptMode, RepeatMode};

        let builtin = load(ConfigProduct::Dalgon, "").unwrap();
        assert_eq!(
            builtin.rules(),
            &RulesConfig {
                watch: true,
                interrupt: InterruptMode::Always,
                repeat: RepeatMode::Once,
                repeat_gap: 10,
                max_retries: 3,
                disabled: Vec::new(),
                judge: JudgeMode::Auto,
            }
        );

        let defaults = "[rules]\nrepeat = \"after-gap\"\nrepeat_gap = 5\ninterrupt = \"never\"\ndisabled = [\"a\"]\n";
        let config = Config::load(
            ConfigProduct::Dalgona,
            Path::new(DATA_ROOT),
            defaults,
            Some("[rules]\nrepeat_gap = 7\nmax_retries = 0\njudge = \"off\"\nwatch = false\n"),
        )
        .unwrap();
        assert_eq!(
            config.rules(),
            &RulesConfig {
                watch: false,
                interrupt: InterruptMode::Never,
                repeat: RepeatMode::AfterGap,
                repeat_gap: 7,
                max_retries: 0,
                disabled: vec![Box::from("a")],
                judge: JudgeMode::Off,
            }
        );

        let replaced = Config::load(
            ConfigProduct::Dalgon,
            Path::new(DATA_ROOT),
            defaults,
            Some("[rules]\ndisabled = []"),
        )
        .unwrap();
        assert!(replaced.rules().disabled.is_empty());
        assert_eq!(replaced.rules().repeat_gap, 5);

        let top_level_judge = load(ConfigProduct::Dalgon, "judge = \"off\"").unwrap();
        assert_eq!(top_level_judge.rules().judge, JudgeMode::Auto);
    }

    #[test]
    fn rules_integer_bounds_are_inclusive_and_closed() {
        for (key, valid, invalid) in [
            (
                "repeat_gap",
                ["1", "1000"],
                ["0", "1001", "-1", "\"10\"", "10.0"],
            ),
            (
                "max_retries",
                ["0", "20"],
                ["21", "-1", "99999999999", "true", "3.0"],
            ),
        ] {
            for value in valid {
                let document = format!("[rules]\n{key} = {value}");
                assert!(load(ConfigProduct::Dalgon, &document).is_ok(), "{document}");
            }
            for value in invalid {
                let document = format!("[rules]\n{key} = {value}");
                assert!(
                    matches!(
                        load(ConfigProduct::Dalgon, &document),
                        Err(ConfigError::InvalidValue { key: found, .. })
                            if found.as_ref() == format!("rules.{key}")
                    ),
                    "{document}"
                );
            }
        }
    }

    #[test]
    fn rules_disabled_names_follow_rule_name_grammar() {
        let longest = format!("a{}", "b".repeat(63));
        let valid =
            format!("[rules]\ndisabled = [\"no-sleep\", \"A.b_c-1\", \"0\", \"{longest}\"]");
        let config = load(ConfigProduct::Dalgon, &valid).unwrap();
        assert_eq!(config.rules().disabled.len(), 4);
        assert_eq!(config.rules().disabled[3].as_ref(), longest);

        let too_long = format!("[\"{longest}c\"]");
        for value in [
            "[\"Bad Name\"]",
            "[\"\"]",
            "[\".hidden\"]",
            "[\"-x\"]",
            "[\"_x\"]",
            "[\"caf\u{e9}\"]",
            "[\"ok\", 1]",
            "\"no-sleep\"",
            too_long.as_str(),
        ] {
            let document = format!("[rules]\ndisabled = {value}");
            assert!(
                matches!(
                    load(ConfigProduct::Dalgon, &document),
                    Err(ConfigError::InvalidValue { key, .. }) if key.as_ref() == "rules.disabled"
                ),
                "{document}"
            );
        }
    }

    #[test]
    fn rules_errors_print_the_exact_two_lines() {
        for (body, first, second) in [
            (
                "interrupt = \"sometimes\"",
                "dalgon: config.toml: rules.interrupt \"sometimes\" is invalid",
                "Use one of always, prose-only, tool-only, never.",
            ),
            (
                "repeat = \"twice\"",
                "dalgon: config.toml: rules.repeat \"twice\" is invalid",
                "Use one of once, after-gap.",
            ),
            (
                "repeat_gap = 0",
                "dalgon: config.toml: rules.repeat_gap 0 is invalid",
                "Use a whole number from 1 to 1000.",
            ),
            (
                "max_retries = 50",
                "dalgon: config.toml: rules.max_retries 50 is invalid",
                "Use a whole number from 0 to 20.",
            ),
            (
                "watch = \"yes\"",
                "dalgon: config.toml: rules.watch \"yes\" is invalid",
                "Use true or false.",
            ),
            (
                "disabled = [\"Bad Name\"]",
                "dalgon: config.toml: rules.disabled [\"Bad Name\"] is invalid",
                "Use a list of rule names, such as [\"no-sleep\"].",
            ),
            (
                "judge = \"maybe\"",
                "dalgon: config.toml: rules.judge \"maybe\" is invalid",
                "Use one of auto, on, off.",
            ),
        ] {
            assert_eq!(
                rules_error_lines(ConfigProduct::Dalgon, body),
                [first.to_owned(), second.to_owned()]
            );
        }
        assert_eq!(
            rules_error_lines(ConfigProduct::Dalgona, "repeat = \"twice\""),
            [
                "dalgona: config.toml: rules.repeat \"twice\" is invalid".to_owned(),
                "Use one of once, after-gap.".to_owned(),
            ]
        );
        let other = load(ConfigProduct::Dalgon, "mode = \"jit\"").unwrap_err();
        assert_eq!(other.rules_lines("dalgon"), None);
    }

    #[test]
    fn rules_table_keys_are_closed() {
        let error = load(ConfigProduct::Dalgon, "[rules]\nwach = true").unwrap_err();
        assert!(matches!(
            error,
            ConfigError::UnknownKey { key, suggestion, .. }
                if key.as_ref() == "rules.wach" && suggestion.as_deref() == Some("rules.watch")
        ));
        assert!(matches!(
            load(ConfigProduct::Dalgona, "rules = 1"),
            Err(ConfigError::InvalidValue { key, .. }) if key.as_ref() == "rules"
        ));
    }
}
