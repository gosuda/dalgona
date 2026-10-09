//! Terminal sign-in: the provider rows, the API-key prompt, the OAuth flow
//! overlay, and the remove-all confirmation.
//!
//! The flow itself runs in the host ([`TuiHost::login`]). This module only
//! shows its progress, owns the one pasted value, and cancels it. A login
//! runs on its own thread because the terminal loop is synchronous; the
//! thread drives the host future on the process-edge runtime and reports
//! through a channel the loop drains each tick.

use std::sync::mpsc;
use std::thread::JoinHandle;

use dal_agent::login::{
    CancellationToken, CredentialKind, LoginIo, LoginOutcome, LoginProgress, Method, SecretString,
    StoredCredential, login_providers,
};
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use crate::TuiError;
use crate::backend::TuiHost;
use crate::copy::{ids, render};
use crate::keys::Key;
use crate::picker::{PickerAction, PickerOption, PickerUi};
use crate::render::{RenderLink, RenderRow};
use crate::term::TermIo;
use crate::theme::Role;
use crate::width::{WidthMode, escape};

/// One event from a running login thread.
enum RunEvent {
    Progress(LoginProgress),
    Done(Box<Result<LoginOutcome, TuiError>>),
}

/// A login running on its own thread.
struct LoginRun {
    events: mpsc::Receiver<RunEvent>,
    cancel: CancellationToken,
    paste: Option<oneshot::Sender<String>>,
    thread: Option<JoinHandle<()>>,
}

impl LoginRun {
    /// Starts `method` for `provider`. `key` is the API key for
    /// [`Method::ApiKey`]; every other method leaves the paste channel open
    /// for one pasted value.
    fn start<H: TuiHost>(
        host: &H,
        rt: &Handle,
        provider: &str,
        method: Method,
        key: Option<SecretString>,
    ) -> Self {
        let (events_tx, events) = mpsc::channel();
        let (paste_tx, paste_rx) = oneshot::channel();
        let paste = match key {
            Some(key) => {
                // A fresh channel's receiver is alive, so this send cannot fail.
                let _delivered = paste_tx.send(key.expose().to_owned());
                None
            }
            None => Some(paste_tx),
        };
        let cancel = CancellationToken::new();
        let host = host.clone();
        let rt = rt.clone();
        let provider: Box<str> = provider.into();
        let io_cancel = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("dal-tui-login".to_owned())
            .spawn(move || {
                let (io, mut progress) = LoginIo::channel(Some(paste_rx), io_cancel);
                let outcome = rt.block_on(async {
                    let forward = async {
                        while let Some(event) = progress.recv().await {
                            if events_tx.send(RunEvent::Progress(event)).is_err() {
                                break;
                            }
                        }
                    };
                    let (outcome, ()) = tokio::join!(host.login(&provider, method, io), forward);
                    outcome
                });
                let _closed = events_tx.send(RunEvent::Done(Box::new(outcome)));
            })
            .ok();
        Self {
            events,
            cancel,
            paste,
            thread,
        }
    }

    /// Sends the one pasted value; false when the flow takes none.
    fn send_paste(&mut self, text: String) -> bool {
        self.paste
            .take()
            .is_some_and(|sender| sender.send(text).is_ok())
    }
}

impl Drop for LoginRun {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

/// What the flow overlay knows so far.
#[derive(Default)]
struct FlowView {
    url: Option<String>,
    code: Option<String>,
    opened: bool,
    paste_hint: Option<String>,
    pasted: String,
    exchanging: bool,
    cancelling: bool,
}

enum Stage {
    /// The masked API-key prompt.
    Key { typed: String },
    /// A running OAuth flow.
    Flow(Box<FlowView>),
    /// The remove-all confirmation.
    ConfirmAll,
}

/// The state change a key asks the loop to make.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeyAction {
    /// Nothing outside the overlay changes.
    None,
    /// Close the overlay.
    Close,
    /// Start the key login with the typed key.
    StartKey(SecretString),
    /// Remove every stored credential.
    RemoveAll,
}

/// How a login ended.
pub(crate) struct Finished {
    /// The provider that was signed in to.
    pub(crate) provider: Box<str>,
    /// The host's outcome.
    pub(crate) outcome: Result<LoginOutcome, TuiError>,
}

/// The sign-in overlay: modal over the composer, like the pickers.
pub(crate) struct SignIn {
    provider: Box<str>,
    stage: Stage,
    run: Option<LoginRun>,
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SignIn")
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// The method the terminal signs in with: the browser when the provider
/// offers it, else a pasted key.
pub(crate) fn preferred_method(provider: &str) -> Option<Method> {
    let (_, methods) = login_providers().iter().find(|(id, _)| *id == provider)?;
    [Method::Browser, Method::ApiKey]
        .into_iter()
        .find(|method| methods.contains(method))
}

impl SignIn {
    /// The masked API-key prompt for `provider`.
    pub(crate) fn key_prompt(provider: &str) -> Self {
        Self {
            provider: provider.into(),
            stage: Stage::Key {
                typed: String::new(),
            },
            run: None,
        }
    }

    /// The remove-all confirmation.
    pub(crate) fn confirm_all() -> Self {
        Self {
            provider: "".into(),
            stage: Stage::ConfirmAll,
            run: None,
        }
    }

    /// Starts a flow for `provider` and shows it.
    pub(crate) fn flow<H: TuiHost>(
        host: &H,
        rt: &Handle,
        provider: &str,
        method: Method,
        key: Option<SecretString>,
    ) -> Self {
        Self {
            provider: provider.into(),
            stage: Stage::Flow(Box::default()),
            run: Some(LoginRun::start(host, rt, provider, method, key)),
        }
    }

    /// The provider this overlay signs in to.
    pub(crate) fn provider(&self) -> &str {
        &self.provider
    }

    /// Whether a login is running.
    pub(crate) fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Adds pasted text to the open input, one line.
    pub(crate) fn paste(&mut self, text: &str) {
        let line: String = text.chars().filter(|c| !matches!(c, '\r' | '\n')).collect();
        match &mut self.stage {
            Stage::Key { typed } => typed.push_str(&line),
            Stage::Flow(flow) if flow.paste_hint.is_some() => flow.pasted.push_str(&line),
            Stage::Flow(_) | Stage::ConfirmAll => {}
        }
    }

    /// Applies one key to the overlay.
    pub(crate) fn key(&mut self, key: Key) -> KeyAction {
        use crossterm::event::{KeyCode, KeyModifiers};

        let plain = key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT;
        match &mut self.stage {
            Stage::ConfirmAll => match key.code {
                KeyCode::Char('y' | 'Y') if plain => KeyAction::RemoveAll,
                KeyCode::Char('n' | 'N') | KeyCode::Esc => KeyAction::Close,
                _ => KeyAction::None,
            },
            Stage::Key { typed } => match key.code {
                KeyCode::Esc => KeyAction::Close,
                KeyCode::Enter if !typed.trim().is_empty() => {
                    KeyAction::StartKey(std::mem::take(typed).into())
                }
                KeyCode::Backspace => {
                    crate::composer::pop_grapheme(typed);
                    KeyAction::None
                }
                KeyCode::Char(character) if plain => {
                    typed.push(character);
                    KeyAction::None
                }
                _ => KeyAction::None,
            },
            Stage::Flow(flow) => match key.code {
                KeyCode::Esc => {
                    self.cancel();
                    KeyAction::None
                }
                KeyCode::Enter if flow.paste_hint.is_some() && !flow.pasted.trim().is_empty() => {
                    let text = std::mem::take(&mut flow.pasted);
                    if let Some(run) = &mut self.run
                        && run.send_paste(text)
                    {
                        flow.paste_hint = None;
                        flow.exchanging = true;
                    }
                    KeyAction::None
                }
                KeyCode::Backspace if flow.paste_hint.is_some() => {
                    crate::composer::pop_grapheme(&mut flow.pasted);
                    KeyAction::None
                }
                KeyCode::Char(character) if plain && flow.paste_hint.is_some() => {
                    flow.pasted.push(character);
                    KeyAction::None
                }
                _ => KeyAction::None,
            },
        }
    }

    /// Cancels a running flow. The overlay stays until the flow reports its
    /// one outcome, so cancellation is never shown as success.
    pub(crate) fn cancel(&mut self) {
        if let (Some(run), Stage::Flow(flow)) = (&self.run, &mut self.stage) {
            run.cancel.cancel();
            flow.cancelling = true;
        }
    }

    /// Drains the login thread. Returns the outcome once the flow ended.
    pub(crate) fn poll(&mut self, io: &dyn TermIo) -> Option<Finished> {
        loop {
            let event = self.run.as_ref()?.events.try_recv();
            match event {
                Ok(RunEvent::Progress(progress)) => self.progress(progress, io),
                Ok(RunEvent::Done(outcome)) => {
                    self.run = None;
                    return Some(Finished {
                        provider: self.provider.clone(),
                        outcome: *outcome,
                    });
                }
                Err(mpsc::TryRecvError::Empty) => return None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.run = None;
                    return Some(Finished {
                        provider: self.provider.clone(),
                        outcome: Err(TuiError::Terminal(String::from(
                            "the sign-in thread stopped without an outcome",
                        ))),
                    });
                }
            }
        }
    }

    fn progress(&mut self, progress: LoginProgress, io: &dyn TermIo) {
        let Stage::Flow(flow) = &mut self.stage else {
            return;
        };
        match progress {
            LoginProgress::OpenUrl { url } => {
                flow.opened = io.open_url(&url).is_ok();
                flow.url = Some(url);
            }
            LoginProgress::ShowCode { url, code } => {
                flow.opened = io.open_url(&url).is_ok();
                flow.url = Some(url);
                flow.code = Some(code);
            }
            LoginProgress::AskPaste { hint } => {
                if self.run.as_ref().is_some_and(|run| run.paste.is_some()) {
                    flow.paste_hint = Some(hint);
                }
            }
            LoginProgress::Exchanging => {
                flow.exchanging = true;
                flow.paste_hint = None;
            }
        }
    }

    /// The overlay rows for `width` columns, at most `max_rows` when the URL
    /// can give rows back.
    pub(crate) fn rows(&self, width: usize, mode: WidthMode, max_rows: usize) -> Vec<RenderRow> {
        let provider = escape(&self.provider);
        match &self.stage {
            Stage::ConfirmAll => vec![
                RenderRow::plain(ids::LOGOUT_ALL_TITLE, Role::Accent),
                RenderRow::plain(
                    format!(
                        "[y] {}   [n] {}",
                        ids::LOGOUT_ALL_REMOVE,
                        ids::LOGOUT_ALL_KEEP
                    ),
                    Role::Accent,
                ),
            ],
            Stage::Key { typed } => {
                let shown = typed.chars().count().min(width.saturating_sub(3).max(1));
                let mut input = RenderRow::plain(format!("> {}", "*".repeat(shown)), Role::Text);
                input.cursor = Some(2 + shown);
                vec![
                    RenderRow::new(
                        render(ids::LOGIN_KEY_TITLE, &[("provider", &provider)], 1),
                        Role::Accent,
                    ),
                    input,
                    RenderRow::plain(ids::LOGIN_KEY_HINT, Role::Dim),
                ]
            }
            Stage::Flow(flow) => flow_rows(&provider, flow, width, mode, max_rows),
        }
    }
}

fn flow_rows(
    provider: &str,
    flow: &FlowView,
    width: usize,
    mode: WidthMode,
    max_rows: usize,
) -> Vec<RenderRow> {
    let mut head = vec![RenderRow::new(
        render(ids::LOGIN_TITLE, &[("provider", provider)], 1),
        Role::Accent,
    )];
    let mut url_rows = Vec::new();
    if let Some(url) = &flow.url {
        head.push(RenderRow::plain(ids::LOGIN_OPEN_URL, Role::Text));
        url_rows = url_chunks(url, width, mode)
            .into_iter()
            .map(|chunk| {
                let mut row = RenderRow::plain(chunk, Role::Text);
                row.links.push(RenderLink {
                    range: 0..row.text.len(),
                    url: url.clone(),
                });
                row
            })
            .collect();
    }
    let mut tail = Vec::new();
    if let Some(code) = &flow.code {
        tail.push(RenderRow::plain(
            render(ids::LOGIN_DEVICE, &[("code", &escape(code))], 1),
            Role::Accent,
        ));
    }
    if flow.opened {
        tail.push(RenderRow::plain(ids::LOGIN_OPENED, Role::Dim));
    }
    if let Some(hint) = &flow.paste_hint {
        tail.push(RenderRow::plain(escape(hint), Role::Text));
        let room = width.saturating_sub(3).max(1);
        let count = flow.pasted.chars().count();
        let visible: String = flow
            .pasted
            .chars()
            .skip(count.saturating_sub(room))
            .collect();
        let visible = escape(&visible);
        let mut input = RenderRow::plain(format!("> {visible}"), Role::Text);
        input.cursor = Some(2 + crate::width::width(&visible, mode));
        tail.push(input);
    }
    tail.push(RenderRow::plain(
        if flow.cancelling {
            ids::LOGIN_CANCELLING
        } else if flow.exchanging {
            ids::LOGIN_EXCHANGING
        } else {
            ids::LOGIN_WAITING
        },
        Role::Dim,
    ));
    tail.push(RenderRow::plain(
        if flow.paste_hint.is_some() {
            ids::LOGIN_PASTE_KEYS
        } else {
            ids::LOGIN_KEYS
        },
        Role::Dim,
    ));
    let room = max_rows.saturating_sub(head.len() + tail.len()).max(1);
    if url_rows.len() > room {
        url_rows.truncate(room);
        if let Some(last) = url_rows.last_mut() {
            last.text = crate::width::take_cells(&last.text, width.saturating_sub(1), mode) + "…";
            last.links.clear();
            last.links.push(RenderLink {
                range: 0..last.text.len(),
                url: flow.url.clone().unwrap_or_default(),
            });
        }
    }
    head.into_iter().chain(url_rows).chain(tail).collect()
}

/// Splits `url` into rows of at most `width` cells.
fn url_chunks(url: &str, width: usize, mode: WidthMode) -> Vec<String> {
    let limit = width.max(1);
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for character in url.chars() {
        let cells = crate::width::width(character.encode_utf8(&mut [0; 4]), mode);
        if !current.is_empty() && used + cells > limit {
            rows.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push(character);
        used += cells;
    }
    if !current.is_empty() {
        rows.push(current);
    }
    rows
}

/// The provider picker for `/login`, from the providers dal signs in to.
pub(crate) fn login_picker(filter: &str) -> PickerUi {
    let options = login_providers()
        .iter()
        .map(|(id, _)| PickerOption {
            label: (*id).to_owned(),
            action: PickerAction::Command(dal_core::Command::Run {
                name: "login".into(),
                args: (*id).into(),
                expected: None,
            }),
        })
        .collect();
    PickerUi::new(ids::LOGIN_PICKER_TITLE, filter, options)
}

/// The kind word of a stored credential, as `dalgon login status` prints it.
fn kind_word(kind: CredentialKind) -> &'static str {
    match kind {
        CredentialKind::ApiKey => "api_key",
        CredentialKind::OAuth => "oauth",
    }
}

/// The picker for `/logout`: one row per stored credential, then
/// `All providers`.
pub(crate) fn logout_picker(stored: &[StoredCredential], filter: &str) -> PickerUi {
    let mut options: Vec<PickerOption> = stored
        .iter()
        .map(|row| PickerOption {
            label: render(
                ids::LOGOUT_ROW,
                &[("provider", &row.provider), ("kind", kind_word(row.kind))],
                1,
            ),
            action: PickerAction::Command(dal_core::Command::Run {
                name: "logout".into(),
                args: row.provider.clone(),
                expected: None,
            }),
        })
        .collect();
    options.push(PickerOption {
        label: ids::LOGOUT_ALL_ROW.to_owned(),
        action: PickerAction::ConfirmLogoutAll,
    });
    PickerUi::new(ids::LOGOUT_PICKER_TITLE, filter, options)
}

/// The output rows after a logout, with the saved-model warning when the
/// saved model needs a removed credential.
pub(crate) fn logout_rows(
    asked: Option<&str>,
    removed: &[Box<str>],
    saved_model: Option<&str>,
) -> Vec<String> {
    if removed.is_empty() {
        return vec![ids::LOGOUT_NONE.to_owned()];
    }
    let mut rows = vec![match asked {
        Some(provider) => render(ids::LOGOUT_REMOVED, &[("provider", provider)], 1),
        None => ids::LOGOUT_REMOVED_ALL.to_owned(),
    }];
    if let Some((id, provider)) = saved_model.and_then(|model| {
        let (provider, _) = model.split_once('/')?;
        removed
            .iter()
            .find(|name| ***name == *provider)
            .map(|_| (model, provider))
    }) {
        rows.push(render(
            ids::LOGOUT_SAVED_MODEL,
            &[("id", id), ("provider", provider)],
            1,
        ));
    }
    rows
}

/// Rows for a failed sign-in: the error text, then its fix.
pub(crate) fn failure_rows(error: &TuiError) -> Vec<String> {
    let (text, fix) = match error {
        TuiError::Host(dal_agent::HostError::Provider(error)) => (error.to_string(), error.fix()),
        TuiError::Backend(error) => (error.to_string(), None),
        other => (other.to_string(), None),
    };
    std::iter::once(escape(&text))
        .chain(fix.map(|fix| escape(&fix)))
        .collect()
}

/// Width of `text` in cells, for the URL wrapper test.
#[cfg(test)]
fn cells(text: &str) -> usize {
    crate::width::width(text, WidthMode::Narrow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_wrap_by_cells_and_keep_every_character() {
        let url = "https://example.test/oauth/authorize?client_id=abc&state=0123456789";
        let rows = url_chunks(url, 16, WidthMode::Narrow);
        assert!(rows.iter().all(|row| cells(row) <= 16), "{rows:?}");
        assert_eq!(rows.concat(), url);
        assert!(rows.len() >= 4);
    }

    #[test]
    fn the_method_prefers_the_browser_then_the_key() {
        assert_eq!(preferred_method("anthropic"), Some(Method::Browser));
        assert_eq!(preferred_method("openai-codex"), Some(Method::Browser));
        assert_eq!(preferred_method("openai"), Some(Method::ApiKey));
        assert_eq!(preferred_method("nobody"), None);
    }

    #[test]
    fn the_logout_rows_name_the_saved_model_only_when_it_needs_a_removed_credential() {
        let removed = [Box::<str>::from("openai-codex")];
        assert_eq!(
            logout_rows(Some("openai-codex"), &removed, Some("openai-codex/gpt-5")),
            [
                "Removed credentials for openai-codex.",
                "The saved model \"openai-codex/gpt-5\" needs openai-codex credentials. Type /model to pick another model.",
            ]
        );
        assert_eq!(
            logout_rows(Some("openai-codex"), &removed, Some("anthropic/claude")),
            ["Removed credentials for openai-codex."]
        );
        assert_eq!(
            logout_rows(None, &removed, None),
            ["Removed all credentials."]
        );
        assert_eq!(
            logout_rows(None, &[], Some("openai/gpt-5")),
            [ids::LOGOUT_NONE]
        );
    }
}
