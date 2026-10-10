//! The `dalgon` argument tree: flags, value enums, and subcommands.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use super::texts;

#[derive(Debug, Parser)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "plan fixes one flag per CLI switch"
)]
#[command(name = "dalgon", version, about = texts::ROOT_HELP, disable_help_flag = false, disable_version_flag = false)]
pub(crate) struct Cli {
    #[arg(short = 'p', long, help = texts::PRINT_HELP)]
    pub(crate) print: bool,
    #[arg(long, help = texts::JSON_HELP)]
    pub(crate) json: bool,
    #[arg(short = 'o', long, value_name = "FILE", help = texts::OUTPUT_LAST_MESSAGE_HELP)]
    pub(crate) output_last_message: Option<PathBuf>,
    #[arg(short = 'c', long = "continue", help = texts::CONTINUE_HELP)]
    pub(crate) continue_session: bool,
    #[arg(short = 'r', long, num_args = 0..=1, help = texts::RESUME_HELP)]
    #[expect(
        clippy::option_option,
        reason = "plan fixes absent/present/value triple for --resume"
    )]
    pub(crate) resume: Option<Option<String>>,
    #[arg(short = 'n', long, help = texts::NAME_HELP)]
    pub(crate) name: Option<String>,
    #[arg(short = 'm', long, value_name = "ID", help = texts::MODEL_HELP)]
    pub(crate) model: Option<String>,
    #[arg(long, value_enum, help = texts::MODE_HELP)]
    pub(crate) mode: Option<ModeArg>,
    #[arg(long, value_enum, help = texts::THINKING_HELP)]
    pub(crate) thinking: Option<ThinkingArg>,
    #[arg(long, value_enum, help = texts::APPROVAL_HELP)]
    pub(crate) approval: Option<ApprovalArg>,
    #[arg(long, help = texts::SANDBOX_HELP)]
    pub(crate) sandbox: bool,
    #[arg(long, help = texts::NO_SESSION_HELP)]
    pub(crate) no_session: bool,
    #[arg(long, value_enum, help = texts::SCREEN_HELP)]
    pub(crate) screen: Option<ScreenArg>,
    #[arg(long, value_enum, help = texts::COLOR_HELP)]
    pub(crate) color: Option<ColorArg>,
    #[arg(short = 'C', long, value_name = "DIR", help = texts::CD_HELP)]
    pub(crate) cd: Option<PathBuf>,
    #[arg(long, value_name = "ADDR", help = texts::CONNECT_HELP)]
    pub(crate) connect: Option<String>,
    #[arg(
        long = "token-file",
        value_name = "FILE",
        requires = "connect",
        help = texts::CONNECT_TOKEN_FILE_HELP
    )]
    pub(crate) connect_token_file: Option<PathBuf>,
    #[arg(value_name = "PROMPT", help = texts::PROMPTS_HELP)]
    pub(crate) prompts: Vec<OsString>,
    #[command(subcommand)]
    pub(crate) command: Option<Commands>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum ModeArg {
    Normal,
    EvalFirst,
    EvalOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum ThinkingArg {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum ApprovalArg {
    Ask,
    Edits,
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum ScreenArg {
    Inline,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum ColorArg {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum ShellArg {
    Bash,
    Elvish,
    Fish,
    Powershell,
    Zsh,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    #[command(about = texts::LOGIN_HELP)]
    Login(LoginArgs),
    #[command(about = texts::LOGOUT_HELP)]
    Logout(ProviderArgs),
    #[command(about = texts::MODELS_HELP)]
    Models(ModelsArgs),
    #[command(about = texts::DOCS_HELP)]
    Docs(DocsArgs),
    #[command(about = texts::SERVE_HELP)]
    Serve(ServeCliArgs),
    #[command(about = texts::RPC_HELP)]
    Rpc(RpcArgs),
    #[command(about = texts::ACP_HELP)]
    Acp,
    #[command(name = "app-server", about = texts::APP_SERVER_HELP)]
    AppServer,
    #[command(about = texts::COMPLETION_HELP)]
    Completion(CompletionArgs),
    #[command(about = texts::PLUGIN_HELP)]
    Plugin(PluginArgs),
    #[command(about = texts::RULES_HELP)]
    Rules(RulesArgs),
    #[command(about = texts::DEV_HELP)]
    Dev(DevArgs),
    #[command(name = "__sandbox", hide = true, about = texts::SANDBOX_HELP)]
    Sandbox,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct LoginArgs {
    #[arg(value_name = "PROVIDER")]
    pub(crate) provider: Option<String>,
    #[arg(long, help = texts::LOGIN_API_KEY_HELP)]
    pub(crate) api_key: bool,
    #[arg(long, help = texts::LOGIN_DEVICE_AUTH_HELP)]
    pub(crate) device_auth: bool,
    #[command(subcommand)]
    pub(crate) command: Option<LoginSubcommand>,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum LoginSubcommand {
    #[command(about = texts::LOGIN_STATUS_HELP)]
    Status,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct ProviderArgs {
    #[arg(value_name = "PROVIDER")]
    pub(crate) provider: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct ModelsArgs {
    #[arg(value_name = "PATTERN", help = texts::MODELS_PATTERN_HELP)]
    pub(crate) pattern: Option<String>,
    #[arg(long, help = texts::MODELS_JSON_HELP)]
    pub(crate) json: bool,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct DocsArgs {
    #[arg(value_name = "URI")]
    pub(crate) uri: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct ServeCliArgs {
    #[arg(long, value_name = "ADDR", help = texts::SERVE_BIND_HELP)]
    pub(crate) bind: Option<String>,
    #[arg(long, value_name = "PORT", help = texts::SERVE_PORT_HELP)]
    pub(crate) port: Option<u16>,
    #[arg(long, help = texts::SERVE_PUBLIC_HELP)]
    pub(crate) public: bool,
    #[arg(long, value_name = "FILE", help = texts::SERVE_TOKEN_FILE_HELP)]
    pub(crate) token_file: Option<PathBuf>,
    #[arg(long, help = texts::SERVE_A2A_HELP)]
    pub(crate) a2a: bool,
    #[command(subcommand)]
    pub(crate) command: Option<ServeSubcommand>,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum ServeSubcommand {
    #[command(about = texts::SERVE_TOKEN_HELP)]
    Token(ServeTokenArgs),
}

#[derive(Clone, Debug, Args)]
pub(crate) struct ServeTokenArgs {
    #[arg(long, help = texts::SERVE_FORCE_HELP)]
    pub(crate) force: bool,
}

/// The `--socket [FILE]` selection for `dalgon rpc`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RpcSocket {
    /// `--socket` was given without a value: use the default path under
    /// the data root.
    Auto,
    /// `--socket FILE` was given: listen on `FILE`.
    Path(PathBuf),
}

impl std::str::FromStr for RpcSocket {
    type Err = std::convert::Infallible;

    /// Parses one `--socket` value into its choice.
    ///
    /// clap feeds the missing-value sentinel (NUL) when the flag appears
    /// without a value; NUL can never occur in a real `execve` argument,
    /// so the sentinel is unambiguous.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.as_bytes() == [0] {
            Ok(Self::Auto)
        } else {
            Ok(Self::Path(PathBuf::from(value)))
        }
    }
}

#[derive(Clone, Debug, Args)]
pub(crate) struct RpcArgs {
    #[arg(
        long,
        value_name = "FILE",
        num_args = 0..=1,
        default_missing_value = "\u{0}",
        help = texts::RPC_SOCKET_HELP
    )]
    pub(crate) socket: Option<RpcSocket>,
}

#[derive(Debug, Args)]
pub(crate) struct CompletionArgs {
    #[arg(value_enum, value_name = "SHELL")]
    pub(crate) shell: ShellArg,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct PluginArgs {
    #[command(subcommand)]
    pub(crate) command: PluginSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum PluginSubcommand {
    #[command(about = texts::PLUGIN_GRANT_HELP)]
    Grant { name: String },
    #[command(about = texts::PLUGIN_REVOKE_HELP)]
    Revoke { name: String },
    #[command(about = texts::PLUGIN_LIST_HELP)]
    List,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct RulesArgs {
    #[command(subcommand)]
    pub(crate) command: Option<RulesSubcommand>,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum RulesSubcommand {
    #[command(about = texts::RULE_TEST_HELP)]
    Test(RuleTestCliArgs),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub(crate) enum RuleTestSourceArg {
    Text,
    Thinking,
    Tool,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct DevArgs {
    #[command(subcommand)]
    pub(crate) command: DevSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum DevSubcommand {
    #[command(about = texts::DEV_RUN_HELP)]
    Run(DevRunArgs),
    #[command(about = texts::DEV_JOURNAL_HELP)]
    Journal(DevJournalArgs),
    #[command(about = texts::DEV_FOLD_HELP)]
    Fold(DevFoldArgs),
}

#[derive(Clone, Debug, Args)]
pub(crate) struct DevRunArgs {
    /// The scenario file: one step JSON object per nonblank line.
    #[arg(value_name = "FILE")]
    pub(crate) file: PathBuf,
    /// Keep the run's data root and workspace for inspection.
    #[arg(long)]
    pub(crate) keep: bool,
    /// Reuse an earlier kept run root instead of building a fresh one, so
    /// `resume` and `continue` session steps find a populated store.
    #[arg(long, value_name = "DIR")]
    pub(crate) root: Option<PathBuf>,
    /// Permit in-band authorization: `answer` steps, `set_approval`, an
    /// `expect.request` answer, or a config-declared `approval` act with the
    /// operator's privileges, so the scenario file alone cannot grant them.
    #[arg(long)]
    pub(crate) consent: bool,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct DevFoldArgs {
    /// The journal file to explain.
    #[arg(value_name = "FILE")]
    pub(crate) file: PathBuf,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct DevJournalArgs {
    #[command(subcommand)]
    pub(crate) command: DevJournalSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum DevJournalSubcommand {
    #[command(about = texts::DEV_JOURNAL_REPLAY_HELP)]
    Replay(DevReplayArgs),
    #[command(about = texts::DEV_JOURNAL_DIFF_HELP)]
    Diff(DevDiffArgs),
    #[command(about = texts::DEV_JOURNAL_TORN_HELP)]
    Torn(DevTornArgs),
    #[command(about = texts::DEV_JOURNAL_SIDECAR_HELP)]
    Sidecar(DevSidecarArgs),
}

#[derive(Clone, Debug, Args)]
pub(crate) struct DevReplayArgs {
    /// The journal file to fold.
    #[arg(value_name = "FILE")]
    pub(crate) file: PathBuf,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct DevDiffArgs {
    /// The journal whose folded fields print as removed.
    #[arg(value_name = "BEFORE")]
    pub(crate) before: PathBuf,
    /// The journal whose folded fields print as added.
    #[arg(value_name = "AFTER")]
    pub(crate) after: PathBuf,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct DevTornArgs {
    /// The journal to copy.
    #[arg(value_name = "IN")]
    pub(crate) input: PathBuf,
    /// The torn copy to write.
    #[arg(value_name = "OUT")]
    pub(crate) output: PathBuf,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct DevSidecarArgs {
    /// The session directory holding journal.jsonl and its sidecars.
    #[arg(value_name = "DIR")]
    pub(crate) dir: PathBuf,
    /// Dump one sidecar instead of listing them.
    #[arg(value_name = "NAME")]
    pub(crate) name: Option<String>,
}

#[derive(Clone, Debug, Args)]
pub(crate) struct RuleTestCliArgs {
    #[arg(long, value_enum, default_value = "text", help = texts::RULE_TEST_SOURCE_HELP)]
    pub(crate) source: RuleTestSourceArg,
    #[arg(long, value_name = "TOOL", help = texts::RULE_TEST_TOOL_HELP)]
    pub(crate) tool: Option<String>,
    #[arg(long, value_name = "PATH", help = texts::RULE_TEST_PATH_HELP)]
    pub(crate) path: Option<String>,
    #[arg(value_name = "TEXT")]
    pub(crate) text: String,
}
