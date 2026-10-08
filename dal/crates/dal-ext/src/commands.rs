//! First-party slash-command records and their shared command boundary.
/// Built-in commands that report and manage the active session.
pub mod misc;
pub mod model;
pub mod session;
#[cfg(test)]
mod tests;
pub mod tree;

/// The client surface on which a command returns data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scope {
    /// Affects or reports session state.
    Session,
    /// Requests a client action without performing it in the command handler.
    Front,
}

/// The parser contract applied to text following a built-in command name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arity {
    /// No argument text is permitted.
    None,
    /// Zero or one parsed word is permitted.
    Optional,
    /// The trimmed remainder is passed without splitting.
    Raw,
    /// The argument text is discarded without parsing or validation.
    Ignored,
}

/// Ordered metadata used by registration, completion, and command listing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinSpec {
    /// Canonical command name without the leading slash.
    pub name: &'static str,
    /// Completion hint; an empty string means no hint.
    pub hint: &'static str,
    /// One-line command summary.
    pub summary: &'static str,
    /// Descriptive client scope. This is never an authorization check.
    pub scope: Scope,
    /// Whether dispatch requires an idle session before parsing arguments.
    pub idle: bool,
    /// Argument parsing policy.
    pub arity: Arity,
    /// Whether the handler itself starts a single-instance job.
    pub single_instance: bool,
}

/// Built-in commands in their stable display and registration order.
pub const BUILTINS: [BuiltinSpec; 24] = [
    BuiltinSpec {
        name: "settings",
        hint: "",
        summary: "view and change settings",
        scope: Scope::Front,
        idle: false,
        arity: Arity::None,
        single_instance: false,
    },
    BuiltinSpec {
        name: "model",
        hint: "[provider/model]",
        summary: "pick the model",
        scope: Scope::Session,
        idle: false,
        arity: Arity::Optional,
        single_instance: false,
    },
    BuiltinSpec {
        name: "tree",
        hint: "",
        summary: "navigate the session tree",
        scope: Scope::Session,
        idle: true,
        arity: Arity::None,
        single_instance: false,
    },
    BuiltinSpec {
        name: "thinking",
        hint: "[level]",
        summary: "set the thinking level",
        scope: Scope::Session,
        idle: false,
        arity: Arity::Optional,
        single_instance: false,
    },
    BuiltinSpec {
        name: "scoped-models",
        hint: "",
        summary: "choose the models that /model lists",
        scope: Scope::Session,
        idle: false,
        arity: Arity::None,
        single_instance: false,
    },
    BuiltinSpec {
        name: "export",
        hint: "[path]",
        summary: "write the session to a .md or .jsonl file",
        scope: Scope::Session,
        idle: false,
        arity: Arity::Optional,
        single_instance: true,
    },
    BuiltinSpec {
        name: "import",
        hint: "<path>",
        summary: "open a session from a .jsonl file",
        scope: Scope::Front,
        idle: true,
        arity: Arity::Raw,
        single_instance: false,
    },
    BuiltinSpec {
        name: "share",
        hint: "",
        summary: "not in dalgon: use /export",
        scope: Scope::Session,
        idle: false,
        arity: Arity::Ignored,
        single_instance: false,
    },
    BuiltinSpec {
        name: "bug",
        hint: "",
        summary: "not in dalgon: use /export and /session",
        scope: Scope::Session,
        idle: false,
        arity: Arity::Ignored,
        single_instance: false,
    },
    BuiltinSpec {
        name: "copy",
        hint: "",
        summary: "copy the last reply to the clipboard",
        scope: Scope::Front,
        idle: false,
        arity: Arity::None,
        single_instance: false,
    },
    BuiltinSpec {
        name: "name",
        hint: "[name]",
        summary: "set or show the session name",
        scope: Scope::Session,
        idle: false,
        arity: Arity::Raw,
        single_instance: false,
    },
    BuiltinSpec {
        name: "session",
        hint: "",
        summary: "show session details and usage",
        scope: Scope::Session,
        idle: false,
        arity: Arity::None,
        single_instance: false,
    },
    BuiltinSpec {
        name: "changelog",
        hint: "",
        summary: "show what changed in each version",
        scope: Scope::Session,
        idle: false,
        arity: Arity::None,
        single_instance: false,
    },
    BuiltinSpec {
        name: "hotkeys",
        hint: "",
        summary: "show keyboard shortcuts",
        scope: Scope::Front,
        idle: false,
        arity: Arity::None,
        single_instance: false,
    },
    BuiltinSpec {
        name: "fork",
        hint: "",
        summary: "new branch from a previous message",
        scope: Scope::Session,
        idle: true,
        arity: Arity::None,
        single_instance: false,
    },
    BuiltinSpec {
        name: "clone",
        hint: "",
        summary: "copy this session into a new one",
        scope: Scope::Session,
        idle: true,
        arity: Arity::None,
        single_instance: true,
    },
    BuiltinSpec {
        name: "trust",
        hint: "",
        summary: "not in dalgon: plugins load only from the data directory",
        scope: Scope::Session,
        idle: false,
        arity: Arity::Ignored,
        single_instance: false,
    },
    BuiltinSpec {
        name: "login",
        hint: "[provider]",
        summary: "sign in to a provider",
        scope: Scope::Front,
        idle: false,
        arity: Arity::Optional,
        single_instance: false,
    },
    BuiltinSpec {
        name: "logout",
        hint: "[provider]",
        summary: "remove stored credentials",
        scope: Scope::Front,
        idle: false,
        arity: Arity::Optional,
        single_instance: false,
    },
    BuiltinSpec {
        name: "new",
        hint: "",
        summary: "start a new session",
        scope: Scope::Front,
        idle: true,
        arity: Arity::None,
        single_instance: false,
    },
    BuiltinSpec {
        name: "compact",
        hint: "[instructions]",
        summary: "summarize older context now",
        scope: Scope::Session,
        idle: true,
        arity: Arity::Raw,
        single_instance: true,
    },
    BuiltinSpec {
        name: "resume",
        hint: "[id or name]",
        summary: "open another session",
        scope: Scope::Front,
        idle: true,
        arity: Arity::Raw,
        single_instance: false,
    },
    BuiltinSpec {
        name: "reload",
        hint: "",
        summary: "reload plugins",
        scope: Scope::Session,
        idle: false,
        arity: Arity::None,
        single_instance: true,
    },
    BuiltinSpec {
        name: "quit",
        hint: "",
        summary: "quit dalgon",
        scope: Scope::Front,
        idle: false,
        arity: Arity::None,
        single_instance: false,
    },
];

/// The harness mode command follows the 24 row-30 commands.
pub const MODE: BuiltinSpec = BuiltinSpec {
    name: "mode",
    hint: "<normal|eval-first|eval-only>",
    summary: "set the harness mode",
    scope: Scope::Session,
    idle: false,
    arity: Arity::Raw,
    single_instance: false,
};

/// Finds one built-in command or the trailing `mode` record by canonical name.
#[must_use]
pub fn spec(name: &str) -> Option<&'static BuiltinSpec> {
    BUILTINS
        .iter()
        .chain(std::iter::once(&MODE))
        .find(|command| command.name == name)
}

/// Maximum number of bytes in a plugin command summary.
pub const SUMMARY_MAX: usize = 80;
/// Maximum number of bytes in a plugin command argument hint.
pub const HINT_MAX: usize = 40;

/// Plugin registration rejection category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectKind {
    /// The name is owned by a built-in command.
    BuiltinClash,
    /// Two different plugins registered the same name.
    TwoPlugins,
    /// The command name does not match the accepted grammar.
    InvalidName,
    /// One plugin registered the same name twice.
    TwiceInOnePlugin,
    /// The summary or argument hint exceeds its byte limit.
    SummaryOrHintRange,
}

/// Context used to render one registration rejection as an actionable pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rejected<'a> {
    /// Plugin or battery name.
    pub plugin: &'a str,
    /// Absolute path to the plugin's `plugin.star` file.
    pub plugin_star: &'a str,
    /// Rejected command name.
    pub name: &'a str,
    /// Other plugin name, required for [`RejectKind::TwoPlugins`].
    pub other_plugin: Option<&'a str>,
}

/// Returns whether a plugin command name matches the built-in registration grammar.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    if let Some(skill_name) = name.strip_prefix("skill:") {
        return !skill_name.is_empty()
            && skill_name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    }

    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_lowercase())
        && name.len() <= 32
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Renders the exact startup error pair for an invalid plugin command registration.
#[must_use]
pub fn startup_error(kind: RejectKind, rejected: &Rejected<'_>) -> (String, String) {
    match kind {
        RejectKind::BuiltinClash => (
            format!(
                "dalgon: plugin \"{}\" registers /{}: that name belongs to a built-in command",
                rejected.plugin, rejected.name
            ),
            format!(
                "Rename the command in {}, or remove \"{}\" from the plugins key.",
                rejected.plugin_star, rejected.plugin
            ),
        ),
        RejectKind::TwoPlugins => (
            format!(
                "dalgon: plugins \"{}\" and \"{}\" both register /{}",
                rejected.plugin,
                rejected.other_plugin.unwrap_or_default(),
                rejected.name
            ),
            "Rename the command in one of them, or remove one of them from the plugins key."
                .to_owned(),
        ),
        RejectKind::InvalidName => (
            format!(
                "dalgon: plugin \"{}\" registers an invalid command name \"{}\"",
                rejected.plugin, rejected.name
            ),
            "Use lowercase letters, digits, and hyphens, start with a letter, and use at most 32 characters."
                .to_owned(),
        ),
        RejectKind::TwiceInOnePlugin => (
            format!(
                "dalgon: plugin \"{}\" registers /{} twice",
                rejected.plugin, rejected.name
            ),
            format!(
                "Remove one of the two registrations in {}.",
                rejected.plugin_star
            ),
        ),
        RejectKind::SummaryOrHintRange => (
            format!(
                "dalgon: plugin \"{}\" gives /{} an invalid summary",
                rejected.plugin, rejected.name
            ),
            "Use one line of 1 to 80 bytes, and a hint of at most 40 bytes.".to_owned(),
        ),
    }
}
/// Returns the plural suffix for a count.
pub(crate) fn plural(n: u64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Returns the first eight Unicode scalar values of an identifier.
pub(crate) fn id8(id: &str) -> String {
    id.chars().take(8).collect()
}

use std::fmt::Write as _;

pub(crate) fn comma_group(mut n: u64) -> String {
    let mut groups = [0_u64; 7];
    let mut group_count = 0;
    while n >= 1_000 {
        groups[group_count] = n % 1_000;
        group_count += 1;
        n /= 1_000;
    }

    let mut result = String::with_capacity(26);
    let _ = write!(result, "{n}");
    for group in groups[..group_count].iter().rev() {
        let _ = write!(result, ",{group:03}");
    }
    result
}

/// Finds the closest name within edit distance one; equal distances keep input order.
#[must_use]
pub fn suggest_in<'a>(names: &[&'a str], word: &str) -> Option<&'a str> {
    let max_name_length = names
        .iter()
        .map(|name| name.chars().count())
        .max()
        .unwrap_or_default();
    if word.chars().count() > max_name_length.saturating_add(1) {
        return None;
    }

    let mut best_name = None;
    let mut best_distance = 2;
    for name in names {
        if let Some(distance) = distance_at_most_one(name, word)
            && distance < best_distance
        {
            best_distance = distance;
            best_name = Some(*name);
        }
    }
    best_name
}

fn distance_at_most_one(left: &str, right: &str) -> Option<usize> {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > 1 {
        return None;
    }
    // Optimal string alignment: insertion, deletion, substitution, and one
    // adjacent transposition each cost one, so `modle` meets `model` before
    // `mode` and `hgih` meets `high`.
    let mut table = vec![vec![0_usize; right.len() + 1]; left.len() + 1];
    for (row, cell) in table.iter_mut().enumerate() {
        cell[0] = row;
    }
    table[0]
        .iter_mut()
        .enumerate()
        .for_each(|(column, cell)| *cell = column);
    for row in 1..=left.len() {
        for column in 1..=right.len() {
            let cost = usize::from(left[row - 1] != right[column - 1]);
            table[row][column] = (table[row - 1][column] + 1)
                .min(table[row][column - 1] + 1)
                .min(table[row - 1][column - 1] + cost);
            if row > 1
                && column > 1
                && left[row - 1] == right[column - 2]
                && left[row - 2] == right[column - 1]
            {
                table[row][column] = table[row][column].min(table[row - 2][column - 2] + 1);
            }
        }
    }
    let distance = table[left.len()][right.len()];
    (distance <= 1).then_some(distance)
}
/// Renders an import failure with the shared path-check remedy.
#[must_use]
pub fn render_import(failure: &dal_core::command::ImportFailure) -> dal_core::command::ErrorTriple {
    let fix = "Check the path, or write a session with /export PATH.jsonl.";
    match failure {
        dal_core::command::ImportFailure::MissingFile { abs } => error_triple(
            format!("Cannot import {}", abs.display()),
            "the file does not exist",
            fix,
        ),
        dal_core::command::ImportFailure::NotSession { abs, line, reason } => error_triple(
            format!("Cannot import {}", abs.display()),
            format!("it is not a dalgon session file: line {line}: {reason}"),
            fix,
        ),
        dal_core::command::ImportFailure::LineTooLong { abs, line } => error_triple(
            format!("Cannot import {}", abs.display()),
            format!("line {line} is longer than 16 MiB"),
            fix,
        ),
    }
}

/// Converts one structural command rejection into its client-facing prose.
#[must_use]
pub fn render(error: &dal_core::command::CommandError) -> dal_core::command::ErrorTriple {
    use dal_core::command::{BusyState, CommandError};

    match error {
        CommandError::Busy { cmd, state } => match state {
            BusyState::Compacting => error_triple(
                "The session is compacting",
                format!("/{cmd} needs an idle session"),
                "Wait for the compaction to end.",
            ),
            BusyState::Running => error_triple(
                "A turn is running",
                format!("/{cmd} needs an idle session"),
                "Press esc to stop the turn, or wait for it to end.",
            ),
        },
        CommandError::Arity { cmd, args } => error_triple(
            format!("/{cmd} takes no arguments"),
            format!("the text after it was \"{args}\""),
            format!("Type /{cmd} alone."),
        ),
        CommandError::Unknown {
            name,
            suggestion: Some(suggestion),
        } => error_triple(
            format!("Unknown command /{name}"),
            "no built-in or plugin command has this name",
            format!(
                "Did you mean /{suggestion}? Start the message with a space to send it as text."
            ),
        ),
        CommandError::Unknown {
            name,
            suggestion: None,
        } => error_triple(
            format!("Unknown command /{name}"),
            "no built-in or plugin command has this name",
            "Type / to see the commands, or start the message with a space to send it as text.",
        ),
        CommandError::Lex { args, error } => match error.quote() {
            Some(quote) => {
                let kind = if quote == '\'' { "single" } else { "double" };
                error_triple(
                    format!("The arguments have an unclosed {kind} quote"),
                    args.to_string(),
                    "Close the quote, or remove it.",
                )
            }
            None => error_triple(
                "The arguments end with a backslash",
                args.to_string(),
                "Remove the backslash, or add the character it escapes.",
            ),
        },
        CommandError::JobCap { cmd } => error_triple(
            format!("/{cmd} is already running"),
            format!("this session runs one {cmd} at a time"),
            "Wait for it to end.",
        ),
        CommandError::PluginCap => error_triple(
            "Too many plugin commands are running",
            "this session runs at most 16",
            "Wait for one to end.",
        ),
        CommandError::Import(failure) => render_import(failure),
    }
}
/// Renders a plugin handler failure for its tracked job outcome.
#[must_use]
pub fn plugin_raised(cmd: &str, plugin: &str, message: &str) -> dal_core::command::ErrorTriple {
    error_triple(
        format!("/{cmd} failed"),
        format!("the plugin \"{plugin}\" raised {message}"),
        "Report this to the plugin author.",
    )
}

pub(super) fn error_triple(
    what: impl Into<Box<str>>,
    why: impl Into<Box<str>>,
    fix: impl Into<Box<str>>,
) -> dal_core::command::ErrorTriple {
    dal_core::command::ErrorTriple {
        what: what.into(),
        why: why.into(),
        fix: fix.into(),
    }
}

/// Checks a raw command tail against the record's arity contract.
///
/// Returns the single argument a handler consumes: `None` for `None` and
/// `Ignored`, the one word for a bare `Optional`, the tail itself for `Raw`.
/// `Ignored` neither parses nor checks; `Raw` passes through unchecked. Any
/// other parse failure is `Lex`; a second word or any tail where none is
/// allowed is `Arity`. The idle gate runs before this check, so a gated tail
/// during a turn is busy, never arity.
///
/// # Errors
///
/// Returns `Lex` for an unparsable tail and `Arity` when the tail's word
/// count breaks the record's arity.
pub fn check_arity(
    cmd: &str,
    arity: Arity,
    raw: &str,
) -> Result<Option<Box<str>>, dal_core::command::CommandError> {
    use dal_core::command::CommandError;

    match arity {
        Arity::Ignored => Ok(None),
        Arity::Raw => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.into()))
            }
        }
        Arity::None => match dal_core::command::tokens(raw) {
            Err(error) => Err(CommandError::Lex {
                args: raw.into(),
                error,
            }),
            Ok(_) => {
                if raw.trim().is_empty() {
                    Ok(None)
                } else {
                    Err(CommandError::Arity {
                        cmd: cmd.into(),
                        args: raw.trim().into(),
                    })
                }
            }
        },
        Arity::Optional => match dal_core::command::tokens(raw) {
            Err(error) => Err(CommandError::Lex {
                args: raw.into(),
                error,
            }),
            Ok(parts) => {
                if parts.len() > 1 {
                    Err(CommandError::Arity {
                        cmd: cmd.into(),
                        args: raw.trim().into(),
                    })
                } else {
                    Ok(parts.into_iter().next())
                }
            }
        },
    }
}

/// Reloads user plugins and publishes one generation: the `/reload` one path.
///
/// The seam re-runs load, publishes the user-origin set through
/// `CommandCx::publish_plugins`, and reports the published counts. The host
/// validates, builds, and publishes; failures keep the old table live and
/// arrive as the seam's own triple.
pub trait PluginReload: Send + Sync + 'static {
    /// Re-runs load, publishes through the host, and reports the counts.
    fn reload<'a>(
        &'a self,
        cx: &'a dal_agent::ext::command::CommandCx<'a>,
    ) -> dal_agent::ext::BoxFuture<
        'a,
        Result<dal_agent::ext::command::ReloadSummary, dal_core::command::ErrorTriple>,
    >;
}

/// Registers the 25 built-in records as one first-party extension.
///
/// # Errors
///
/// Returns the builder's registration error for an invalid identity.
///
/// Takes the reload seam by value and clones it into each of the 25 command
/// handlers; the public constructor signature is pinned by the external
/// consumers (dalgon product, dal-wire tests).
#[expect(
    clippy::needless_pass_by_value,
    reason = "public constructor takes the seam by value and clones it per registration"
)]
pub fn extension(
    reload: std::sync::Arc<dyn PluginReload>,
) -> Result<dal_agent::ext::Extension, dal_core::RegistrationError> {
    let mut builder = dal_agent::ext::ExtensionBuilder::new(
        "commands",
        env!("CARGO_PKG_VERSION"),
        dal_core::ServiceSet::EMPTY,
    )?;
    for record in BUILTINS.iter().chain(std::iter::once(&MODE)) {
        let name: dal_core::CommandName = record.name.parse()?;
        builder = builder.command(
            dal_core::CommandSpec {
                name,
                summary: record.summary.into(),
                args_hint: if record.hint.is_empty() {
                    None
                } else {
                    Some(record.hint.into())
                },
            },
            std::sync::Arc::new(BuiltinHandler {
                spec: *record,
                reload: reload.clone(),
            }),
        );
    }
    builder.build()
}

/// One first-party command implementation behind the extension registry.
struct BuiltinHandler {
    spec: BuiltinSpec,
    reload: std::sync::Arc<dyn PluginReload>,
}

impl dal_agent::ext::CommandHandler for BuiltinHandler {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: dal_agent::ext::CommandCx<'a>,
    ) -> dal_agent::ext::BoxFuture<'a, Result<dal_core::command::Reply, dal_agent::ServiceError>>
    {
        Box::pin(async move {
            dispatch(self.spec.name, cx, args, &self.reload)
                .await
                .map_err(dal_agent::ServiceError::Command)
        })
    }
}

/// Runs one built-in command tail to a reply.
///
/// Parses arguments once by the record's arity, routes by canonical name,
/// and lifts handler triples into the service error path the dispatcher
/// renders as the `error { what, why, fix }` reply. Unknown names render
/// through the shared unknown pair; the idle gate runs before this call.
///
/// # Errors
///
/// Returns the rendered `ErrorTriple` for an unknown name or an arity
/// failure, and each handler's own triple otherwise.
pub async fn dispatch<'a>(
    name: &str,
    cx: dal_agent::ext::command::CommandCx<'a>,
    tail: &'a str,
    reload: &'a std::sync::Arc<dyn PluginReload>,
) -> Result<dal_core::command::Reply, dal_core::command::ErrorTriple> {
    use dal_core::command::CommandError;

    let Some(record) = spec(name) else {
        let names: Vec<&str> = BUILTINS
            .iter()
            .map(|item| item.name)
            .chain(std::iter::once(MODE.name))
            .collect();
        return Err(render(&CommandError::Unknown {
            name: name.into(),
            suggestion: suggest_in(&names, name).map(Into::into),
        }));
    };
    let arg = match check_arity(name, record.arity, tail) {
        Ok(arg) => arg,
        Err(error) => return Err(render(&error)),
    };
    let word = arg.as_deref();
    let text = word.unwrap_or("");
    match record.name {
        "settings" => Ok(model::settings()),
        "model" => model::model(&cx, word).await,
        "tree" => tree::tree(&cx),
        "thinking" => model::thinking(&cx, word).await,
        "scoped-models" => model::scoped_models(&cx),
        "export" => session::export(&cx, word),
        "import" => session::import(&cx, text),
        "share" => Ok(misc::share()),
        "bug" => Ok(misc::bug()),
        "copy" => session::copy(&cx),
        "name" => session::name(&cx, text).await,
        "session" => Ok(session::details(&cx)),
        "changelog" => Ok(session::changelog(&cx)),
        "hotkeys" => Ok(session::hotkeys()),
        "fork" => tree::fork(&cx),
        "clone" => tree::clone(&cx),
        "trust" => Ok(misc::trust(&cx)),
        "login" => model::login(word),
        "logout" => model::logout(word),
        "new" => Ok(session::new_session()),
        "compact" => misc::compact(&cx, text),
        "resume" => session::resume(&cx, text),
        "reload" => misc::reload(&cx, &**reload).await,
        "quit" => Ok(session::quit(&cx)),
        "mode" => model::mode(&cx, text).await,
        _ => Err(render(&CommandError::Unknown {
            name: name.into(),
            suggestion: None,
        })),
    }
}

/// Completes one built-in command's arguments from cached host data.
///
/// Pure and IO-free; each feed is cut to 50 items. Name ranking and the
/// final cap stay in the registry; `export` and `import` complete nothing.
#[must_use]
pub fn complete_args(
    name: &str,
    prefix: &str,
    cx: &dal_agent::ext::command::CommandCx<'_>,
) -> Vec<dal_core::command::Completion> {
    let feed: Vec<dal_core::command::Completion> = match name {
        "model" => cx
            .catalog()
            .map(|view| {
                view.entries
                    .iter()
                    .map(|entry| dal_core::command::Completion {
                        value: format!("{}/{}", entry.provider, entry.id).into(),
                        label: format!("{}/{}", entry.provider, entry.id).into(),
                        detail: entry.display.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        "thinking" => cx
            .levels_for()
            .iter()
            .map(|level| dal_core::command::Completion {
                value: level.name().into(),
                label: level.name().into(),
                detail: "thinking level".into(),
            })
            .collect(),
        "login" | "logout" => ["anthropic", "openai", "openai-codex"]
            .iter()
            .map(|provider| dal_core::command::Completion {
                value: (*provider).into(),
                label: (*provider).into(),
                detail: "provider".into(),
            })
            .collect(),
        "resume" => {
            let mut names = Vec::new();
            let mut cursor: Option<Box<str>> = None;
            loop {
                let page = cx.sessions_page(50, cursor.as_deref(), None);
                names.extend(page.items.iter().filter_map(|summary| {
                    summary
                        .name
                        .clone()
                        .map(|name| dal_core::command::Completion {
                            value: name.clone(),
                            label: name,
                            detail: summary.id.to_string().into(),
                        })
                }));
                let Some(next) = page.next_before else { break };
                cursor = Some(next);
                if names.len() >= 50 {
                    break;
                }
            }
            names
        }
        _ => Vec::new(),
    };
    feed.into_iter()
        .filter(|item| item.value.starts_with(prefix))
        .take(50)
        .collect()
}
