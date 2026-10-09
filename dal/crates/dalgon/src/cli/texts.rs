//! Shared user-facing text: help lines and two-line command diagnostics.

use std::path::Path;

pub(crate) const ROOT_HELP: &str = "Run the dal coding agent.";
pub(crate) const PRINT_HELP: &str = "Print the assistant response and exit.";
pub(crate) const JSON_HELP: &str = "Print one JSON result object or error object; exit 1 on error.";
pub(crate) const OUTPUT_LAST_MESSAGE_HELP: &str =
    "Write the last assistant message to FILE after a successful run.";
pub(crate) const CONTINUE_HELP: &str = "Continue the newest session in this workspace.";
pub(crate) const RESUME_HELP: &str = "Resume a session by id or name, or open the session picker.";
pub(crate) const NAME_HELP: &str = "Name the session.";
pub(crate) const MODEL_HELP: &str = "Use model ID for this run. ID may name a provider route, an alias, a registered synthetic model, or a dalgon mode.";
pub(crate) const MODE_HELP: &str = "Harness mode for this run: normal, eval-first, or eval-only. The ids dalgon/normal, dalgon/eval-first, and dalgon/eval-only in --model select the same modes.";
pub(crate) const THINKING_HELP: &str = "Set the provider reasoning level.";
pub(crate) const APPROVAL_HELP: &str = "Approval policy for patch and exec tool calls: ask, edits, or all. ask asks in the UI and denies calls in headless runs. edits approves edit calls without asking and asks for exec. all approves every call; use it only inside an external sandbox.";
pub(crate) const SANDBOX_HELP: &str = "Enable the configured sandbox.";
pub(crate) const NO_SESSION_HELP: &str = "Do not persist this session.";
pub(crate) const SCREEN_HELP: &str = "Select the terminal screen mode.";
pub(crate) const COLOR_HELP: &str = "Select terminal color behavior.";
pub(crate) const CD_HELP: &str =
    "Use DIR as the workspace without changing the process working directory.";
pub(crate) const CONNECT_HELP: &str = "Connect to a remote dalgon host and run the interactive terminal UI. Without this option, dalgon uses a host in this process.";
pub(crate) const CONNECT_TOKEN_FILE_HELP: &str = "Read the bearer token from FILE; it is sent only to non-loopback WebSocket endpoints. No token file is inferred.";
pub(crate) const PROMPTS_HELP: &str =
    "Attach text, image, or piped standard-input content to the first user message.";
pub(crate) const LOGIN_HELP: &str = "Sign in to a provider or inspect sign-in state.";
pub(crate) const LOGOUT_HELP: &str = "Sign out from a provider.";
pub(crate) const MODELS_HELP: &str =
    "List models and show built-in prices, price source, and fetch date when available.";
pub(crate) const DOCS_HELP: &str = "Read a documentation page.";
pub(crate) const SERVE_HELP: &str = "Start the local control server or create its token.";
pub(crate) const RPC_HELP: &str = "Serve the JSON-RPC protocol on standard input and output.";
pub(crate) const ACP_HELP: &str = "Serve the Agent Client Protocol on standard input and output.";
pub(crate) const APP_SERVER_HELP: &str =
    "Serve the Codex app-server protocol on standard input and output.";
pub(crate) const COMPLETION_HELP: &str = "Generate shell completion scripts.";
pub(crate) const PLUGIN_HELP: &str = "Manage configured plugin grants.";
pub(crate) const RULES_HELP: &str = "Inspect or test stream rules.";
pub(crate) const LOGIN_API_KEY_HELP: &str = "Read an API key from standard input.";
pub(crate) const LOGIN_DEVICE_AUTH_HELP: &str = "Use the provider device-auth flow.";
pub(crate) const LOGIN_STATUS_HELP: &str = "Show provider sign-in state.";
pub(crate) const LOGIN_PROVIDER_HINT: &str = "Use one of anthropic, openai, or openai-codex.";
pub(crate) const LOGIN_NEEDS_TERMINAL: &str =
    "dalgon: login needs a terminal: no prompt can be shown";
pub(crate) const LOGIN_NEEDS_TERMINAL_HINT: &str =
    "Run it interactively, or pipe the key: printf %s \"$KEY\" | dalgon login anthropic --api-key";
pub(crate) const LOGIN_EMPTY_KEY: &str = "dalgon: no API key was piped: standard input is empty";
pub(crate) const LOGIN_PIPE_KEY_HINT: &str =
    "Pipe the key: printf %s \"$KEY\" | dalgon login PROVIDER --api-key";
pub(crate) const LOGIN_EMPTY_KEY_HINT: &str =
    "Enter a non-empty API key, or pipe one with --api-key.";
pub(crate) const SAVED_ANTHROPIC: &str = "Saved Anthropic credentials. Run dal to pick a model.";
pub(crate) const SAVED_OPENAI: &str = "Saved OpenAI credentials. Run dal to pick a model.";
pub(crate) const SAVED_PROVIDER: &str = "Saved provider credentials.";
pub(crate) const SAVED_CLAUDE: &str = "Signed in to Anthropic.";
pub(crate) const SAVED_CODEX: &str = "Signed in to OpenAI Codex.";
pub(crate) const LOGOUT_ALL: &str = "Removed all credentials.";
pub(crate) const LOGOUT_NONE: &str = "No stored credentials.";
pub(crate) fn saved_model_provider_warning(model: &str, provider: &str) -> String {
    format!(
        "The saved model \"{model}\" needs {provider} credentials. Run dal to pick another model."
    )
}
pub(crate) const DEVICE_AUTH_PROVIDER: &str =
    "dalgon: --device-auth is only valid for openai-codex";
pub(crate) const DEVICE_AUTH_PROVIDER_HINT: &str =
    "Run dalgon login PROVIDER, or dalgon login openai-codex --device-auth.";
pub(crate) const CODEX_API_KEY_UNSUPPORTED: &str =
    "dalgon: openai-codex uses ChatGPT sign-in: API keys are not accepted";
pub(crate) const CODEX_API_KEY_UNSUPPORTED_HINT: &str =
    "Run dalgon login openai-codex, or use an OpenAI API key with dalgon login openai --api-key.";
pub(crate) const AUTH_INVALID_HINT: &str = "Run dalgon login PROVIDER to write the file again.";
pub(crate) const LOGIN_RETRY_HINT: &str = "Run dalgon login PROVIDER and try again.";
pub(crate) const MODELS_FETCH_HINT: &str =
    "Check the network and sign in with dalgon login PROVIDER, then try again.";
pub(crate) const MODELS_PATTERN_HELP: &str = "Filter the model list by PATTERN.";
pub(crate) const MODELS_JSON_HELP: &str = "Print model data as JSON.";
pub(crate) const SERVE_BIND_HELP: &str = "Bind to ADDR instead of the configured address.";
pub(crate) const SERVE_PORT_HELP: &str = "Listen on PORT instead of the configured port.";
pub(crate) const SERVE_PUBLIC_HELP: &str =
    "Expose the listener beyond loopback; clients must use the token.";
pub(crate) const SERVE_TOKEN_FILE_HELP: &str = "Read the serve token from FILE.";
pub(crate) const SERVE_A2A_HELP: &str = "Enable the A2A surface.";
pub(crate) const SERVE_TOKEN_HELP: &str = "Create or replace the serve bearer token.";
pub(crate) const SERVE_FORCE_HELP: &str =
    "Replace an existing token after writing the new token atomically.";
pub(crate) const RPC_SOCKET_HELP: &str = "Listen on a local socket instead of standard input and output. FILE defaults to the per-user socket.";
pub(crate) const PLUGIN_GRANT_HELP: &str = "Grant the configured capabilities declared by NAME.";
pub(crate) const PLUGIN_REVOKE_HELP: &str = "Revoke every persistent grant for NAME.";
pub(crate) const PLUGIN_LIST_HELP: &str = "List configured plugins and their persistent grants.";
pub(crate) const RULE_TEST_HELP: &str = "Simulate a rule against one input.";
pub(crate) const RULE_TEST_SOURCE_HELP: &str = "Select the input stream to test.";
pub(crate) const RULE_TEST_TOOL_HELP: &str =
    "Name the tool for a tool-source test (default: patch).";
pub(crate) const RULE_TEST_PATH_HELP: &str = "Set the path context for a tool-source test.";
pub(crate) const SERVE_PUBLIC_WARNING: &str =
    "dalgon: warning: --public uses plain HTTP; the serve token crosses the network in plain text.";
pub(crate) const SERVE_STOPPED: &str = "dalgon serve stopped.";
pub(crate) const RPC_ONE_JSON_PER_LINE: &str = "dalgon rpc: one JSON object per line.";
pub(crate) const ACP_ONE_MESSAGE_PER_LINE: &str = "dalgon acp: one JSON-RPC message per line.";
#[expect(
    dead_code,
    reason = "shared serve text; consumed by serve.rs once it lands"
)]
pub(crate) const TOKEN_STORED_HELP: &str =
    "Token stored at <path>; clients must send it as a bearer token.";
#[expect(
    dead_code,
    reason = "shared serve text; consumed by serve.rs once it lands"
)]
pub(crate) const DROPPED_LOG_LINES: &str = "dropped <n> log lines";
pub(crate) const MODE_CONFLICT: &str =
    "dalgon: --mode and a dalgon/ mode id in --model cannot be combined";
pub(crate) const MODE_CONFLICT_HINT: &str =
    "Pass the mode once: --mode MODE, or --model dalgon/MODE.";
pub(crate) const CONTINUE_RESUME_CONFLICT: &str =
    "dalgon: --continue and --resume cannot be used together";
pub(crate) const CONNECT_HEADLESS: &str = "dalgon: --connect requires the interactive TUI";
pub(crate) const CONNECT_HEADLESS_HINT: &str =
    "Run dalgon without --print or --json, or omit --connect.";
pub(crate) const DASH_NEEDS_STDIN: &str = "dalgon: \"-\" needs piped standard input";
pub(crate) const DASH_NEEDS_STDIN_HINT: &str =
    "Pipe a prompt: git diff | dalgon -p - \"Review these changes\".";
pub(crate) const HEADLESS_APPROVAL: &str = "dalgon: approval is ask and no terminal is attached: patch and exec calls are denied. Re-run with --approval all to approve them.";

/// The per-call `approval.note_stderr` line: one exact note for each call a
/// headless run could not ask about.
pub(crate) fn approval_denied_note(tool: &str, rung: &str) -> String {
    format!("dalgon: {tool} needs approval; print mode cannot ask. Rerun with --approval {rung}.")
}
pub(crate) const EMPTY_MESSAGE: &str = "dalgon: the model returned an empty message.";

pub(crate) fn serve_token_already_exists(path: &Path) -> [String; 2] {
    [
        format!("dalgon: serve.token already exists: {}", path.display()),
        "Pass --force to replace it. Every client must then use the new token.".into(),
    ]
}

pub(crate) fn serve_public_token_missing(path: &Path) -> [String; 2] {
    [
        format!(
            "dalgon: serve --public needs a token: {} does not exist",
            path.display()
        ),
        "Run dalgon serve token to create it. The token is printed once.".into(),
    ]
}

pub(crate) fn serve_public_token_unsafe(path: &Path, mode: &str) -> [String; 2] {
    [
        format!(
            "dalgon: serve.token is open to other users: {} has mode {mode}",
            path.display()
        ),
        format!(
            "Run chmod 600 {}, or run dalgon serve token --force.",
            path.display()
        ),
    ]
}

pub(crate) fn serve_public_token_empty(path: &Path) -> [String; 2] {
    [
        format!("dalgon: serve.token is empty: {}", path.display()),
        "Run dalgon serve token --force to write a new token.".into(),
    ]
}

pub(crate) fn serve_public_token_invalid(path: &Path) -> [String; 2] {
    [
        format!("dalgon: serve.token is invalid: {}", path.display()),
        "Run dalgon serve token --force to write a new token.".into(),
    ]
}

pub(crate) fn serve_non_loopback_bind(address: &str) -> [String; 2] {
    [
        format!("dalgon: serve --bind {address} is not a loopback address: it needs a token"),
        "Pass --public to require the token from serve.token, or bind 127.0.0.1.".into(),
    ]
}

pub(crate) fn serve_alias_shadows_mode(alias: &str) -> [String; 2] {
    [
        format!("dalgon: [aliases] key \"{alias}\" shadows a dal mode id"),
        "Rename the alias.".into(),
    ]
}

pub(crate) fn serve_bind_failed(address: &str, message: &str) -> [String; 2] {
    [
        format!("dalgon: cannot bind {address}: {message}"),
        "Stop the other process, or pass --port with a free port.".into(),
    ]
}

pub(crate) fn serve_router_refusal(error: &dal_wire::ServeError) -> Option<[String; 2]> {
    match error {
        dal_wire::ServeError::Bind { addr, port, source } => Some(serve_bind_failed(
            &format!("{addr}:{port}"),
            &source.to_string(),
        )),
        dal_wire::ServeError::Resolve { bind, source } => {
            Some(serve_bind_failed(bind, &source.to_string()))
        }
        dal_wire::ServeError::NotLoopback { addr } => {
            Some(serve_non_loopback_bind(&addr.to_string()))
        }
        dal_wire::ServeError::AliasShadowsMode { alias } => Some(serve_alias_shadows_mode(alias)),
        dal_wire::ServeError::Token(error) => serve_token_refusal(error),
        _ => None,
    }
}

fn serve_token_refusal(error: &dal_wire::token::TokenError) -> Option<[String; 2]> {
    match error {
        dal_wire::token::TokenError::Missing { path } => Some(serve_public_token_missing(path)),
        dal_wire::token::TokenError::OpenToOtherUsers { path, mode } => {
            Some(serve_public_token_unsafe(path, &format!("{mode:o}")))
        }
        dal_wire::token::TokenError::Empty { path } => Some(serve_public_token_empty(path)),
        dal_wire::token::TokenError::Invalid { path } => Some(serve_public_token_invalid(path)),
        _ => None,
    }
}

pub(crate) fn serve_advertisement_failed(path: &Path, message: &str) -> [String; 2] {
    [
        format!(
            "dalgon: cannot advertise serve endpoint: {}: {message}",
            path.display()
        ),
        "Check that the data directory is writable.".into(),
    ]
}

pub(crate) fn serve_stored_token(path: &Path) -> String {
    format!("dalgon: stored at {}", path.display())
}

pub(crate) fn serve_listen_line(url: &str, details: &str, a2a: bool) -> String {
    let a2a_suffix = if a2a { ", a2a on" } else { "" };
    format!("dalgon serve listening on {url} ({details}{a2a_suffix})")
}

pub(crate) fn plugin_not_configured(name: &str) -> String {
    format!(
        "dalgon: plugin \"{name}\" is not configured\nAdd \"{name}\" to the plugins key in config.toml, then run dalgon plugin grant {name}."
    )
}
pub(crate) fn plugin_declares_no_services(name: &str) -> String {
    format!("Plugin \"{name}\" declares no services; no grant is needed.")
}

pub(crate) fn plugin_granted(name: &str, origin: &str, services: &str) -> String {
    format!("Granted \"{name}\" ({origin}): {services}.")
}

pub(crate) fn plugin_already_granted(name: &str, origin: &str, services: &str) -> String {
    format!("Already granted \"{name}\" ({origin}): {services}.")
}

pub(crate) fn plugin_no_grants(name: &str) -> String {
    format!("No persistent grants for \"{name}\".")
}

pub(crate) fn plugin_revoked(name: &str, removed: usize) -> String {
    format!(
        "Revoked \"{name}\": removed {removed} persistent grant(s). Session grants remain until their sessions end."
    )
}

pub(crate) fn plugin_list_row(name: &str, origin: &str, services: &str, status: &str) -> String {
    format!("{name} ({origin}): services: {services}; persistent grant: {status}")
}

pub(crate) const PLUGIN_NONE: &str = "No plugins configured.";

pub(crate) const NO_PROMPT: &str =
    "dalgon: no prompt: standard output is not a terminal and no prompt was given";
pub(crate) const NO_PROMPT_HINT: &str =
    "Pipe a prompt on standard input or pass one as an argument.";
pub(crate) const HOME_MISSING_POSIX: &str =
    "dalgon: cannot find the home directory: HOME is not set";
pub(crate) const HOME_MISSING_WINDOWS: &str = "dalgon: cannot find the home directory: none of HOME, USERPROFILE, or HOMEDRIVE+HOMEPATH is set";
pub(crate) const HOME_MISSING_HINT: &str = "Set HOME to your home directory.";
pub(crate) const CONFIG_FIX_HINT: &str =
    "Fix the file, or move it aside and run dalgon to start fresh.";
pub(crate) const HOST_FIX_HINT: &str = "Check the session state and try again.";
pub(crate) const CONNECT_HINT: &str =
    "Connect to the running host with --connect <addr>, or stop that process and try again.";

pub(crate) fn workspace_not_usable(dir: &str) -> [String; 2] {
    [
        format!("dalgon: cannot use \"{dir}\" as the workspace: no such directory"),
        "Run dalgon inside the project directory, or pass -C with an existing directory.".into(),
    ]
}

pub(crate) fn prompt_file_not_found(path: &Path, arg: &str) -> [String; 2] {
    [
        format!("dalgon: file not found: {} (from @{arg})", path.display()),
        "Check the path, or run dalgon from the directory that holds the file.".into(),
    ]
}

pub(crate) fn prompt_file_unreadable(path: &Path, message: &str) -> [String; 2] {
    [
        format!("dalgon: cannot read {}: {message}", path.display()),
        format!(
            "Fix the permissions on {}, or run dalgon from a readable directory.",
            path.display()
        ),
    ]
}

pub(crate) fn prompt_file_not_utf8(path: &Path) -> [String; 2] {
    [
        format!(
            "dalgon: cannot read {}: the file is not valid UTF-8 text",
            path.display()
        ),
        "Pass a UTF-8 text file, or an image ending in .png, .jpg, .jpeg, .gif, or .webp.".into(),
    ]
}

pub(crate) fn status_not_quiet_timeout(busy: &str, seconds: u64) -> [String; 2] {
    let unit = if seconds == 1 { "second" } else { "seconds" };
    [
        format!("dalgon: extension status is not quiet: {busy} still busy after {seconds} {unit}"),
        "Let the extension finish and run dalgon again, or use --json to wait until it is quiet."
            .into(),
    ]
}

pub(crate) fn status_not_quiet_shutdown() -> [String; 2] {
    [
        "dalgon: extension status is not quiet after the shutdown wait".into(),
        "Let the extension finish before closing the session.".into(),
    ]
}

pub(crate) fn stdin_unreadable(message: &str) -> [String; 2] {
    [
        format!("dalgon: cannot read standard input: {message}"),
        "Pipe a prompt: git diff | dalgon -p - \"Review these changes\".".into(),
    ]
}

pub(crate) const CONTINUE_RESUME_HINT: &str = "Pass one of --continue or --resume, not both.";
pub(crate) const EMPTY_NAME: &str = "dalgon: --name cannot be empty";
pub(crate) const EMPTY_NAME_HINT: &str = "Pass a session name, or omit --name.";

pub(crate) const NO_MODEL: &str =
    "dalgon: no model is configured: dal.toml has no model key and --model was not given";
pub(crate) const NO_MODEL_HINT: &str =
    "Run dalgon once and pick a model after sign-in, or pass --model ID. See dalgon models.";

pub(crate) fn terminal_too_narrow(width: usize) -> [String; 2] {
    [
        format!("dalgon: the terminal is {width} columns wide: dalgon needs at least 40"),
        "Widen the terminal, or use print mode: dalgon -p \"...\"".into(),
    ]
}

pub(crate) fn term_not_addressable(value: &str) -> [String; 2] {
    [
        format!("dalgon: TERM is \"{value}\": the interactive UI needs cursor addressing"),
        "Use print mode (dalgon -p), or set TERM to your terminal type.".into(),
    ]
}

pub(crate) fn rpc_socket_dir_open(path: &Path, dir: &Path, mode: u32, data: &Path) -> [String; 2] {
    [
        format!(
            "dalgon: rpc --socket {}: {} is open to other users (mode {mode:04o})",
            path.display(),
            dir.display()
        ),
        format!(
            "Put the socket in a directory of mode 0700, such as {}.",
            data.join("rpc").display()
        ),
    ]
}

pub(crate) fn rpc_socket_path_long(path: &Path, bytes: usize) -> [String; 2] {
    [
        format!(
            "dalgon: rpc --socket {}: the path is {bytes} bytes: a socket path must be at most 103 bytes",
            path.display()
        ),
        "Use a shorter path.".into(),
    ]
}

pub(crate) fn rpc_socket_in_use(path: &Path) -> [String; 2] {
    [
        format!(
            "dalgon: rpc --socket {}: another dalgon rpc is listening there",
            path.display()
        ),
        "Stop it, or pass another --socket path.".into(),
    ]
}

pub(crate) fn rpc_socket_os(path: &Path, message: &str) -> [String; 2] {
    [
        format!("dalgon: rpc --socket {}: {message}", path.display()),
        "Check the path and try again.".into(),
    ]
}

pub(crate) fn internal_error_at(module: &str, message: &str, log_path: &Path) -> [String; 2] {
    [
        format!("dalgon: internal error: {module}: {message}"),
        format!(
            "Report this at https://github.com/gosuda/dalgona/issues with the log at {}.",
            log_path.display()
        ),
    ]
}

pub(crate) fn internal_error(module: &str, message: &str) -> [String; 2] {
    [
        format!("dalgon: internal error: {module}: {message}"),
        "Report this at https://github.com/gosuda/dalgona/issues.".into(),
    ]
}
