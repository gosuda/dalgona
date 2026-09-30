use super::{AgentsConfig, EditStyleInput, GuardSection, PathBuf, PluginLimits, eval::EvalConfig};

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

/// Configuration for optional TUI renderers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TuiConfig {
    /// Whether fenced diagrams render as terminal art.
    pub diagrams: bool,
}

/// System-prompt instructions for supported fenced diagrams.
pub const PROMPT_DIAGRAMS: &str = "Diagrams: put each diagram in a fenced code block tagged `d2`, `nomnoml`, `dot`, or `mermaid`. Prefer `d2` for a new diagram. One diagram per fence. Do not draw diagrams as ASCII art.";

/// The fully resolved product configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent configuration switches have distinct merge and validation rules"
)]
pub struct Config {
    pub(super) mode: Mode,
    pub(super) model: Option<Box<str>>,
    pub(super) thinking: crate::model::ThinkingLevel,
    pub(super) approval: ApprovalMode,
    pub(super) screen: Screen,
    pub(super) tui: TuiConfig,
    pub(super) theme: Box<str>,
    pub(super) sandbox: bool,
    pub(super) sandbox_writable: Vec<Box<str>>,
    pub(super) images: bool,
    pub(super) motion: bool,
    pub(super) compact_ratio: f64,
    pub(super) edit_style: EditStyleInput,
    pub(super) guard: GuardSection,
    pub(super) search_symbols: bool,
    pub(super) plugins: Vec<Box<str>>,
    pub(super) plugin_limits: PluginLimits,
    pub(super) agents: AgentsConfig,
    pub(super) aliases: std::collections::BTreeMap<Box<str>, Box<str>>,
    pub(super) serve: ServeConfig,
    pub(super) prices: std::collections::BTreeMap<Box<str>, crate::model::ModelPrice>,
    pub(super) disabled_batteries: Vec<Box<str>>,
    pub(super) experimental_batteries: Vec<Box<str>>,
    pub(super) rules: RulesConfig,
    /// Effective `[eval]` authority; empty means pure computation only.
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub(super) eval: EvalConfig,
    /// Merged raw TOML for extension-owned tables, keyed by top-level name.
    #[cfg_attr(feature = "schema", schemars(skip))]
    pub(super) sections: std::collections::BTreeMap<Box<str>, toml::Value>,
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
