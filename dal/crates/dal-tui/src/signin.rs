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
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flow flags; a bitset loses legibility"
)]
struct FlowView {
    url: Option<String>,
    /// Whether the URL passed the http(s) check, making it safe to open in
    /// the browser and to emit as a terminal hyperlink.
    url_trusted: bool,
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
    let def = login_providers()
        .into_iter()
        .find(|def| def.id == provider)?;
    [Method::Browser, Method::ApiKey]
        .into_iter()
        .find(|method| def.offers(*method))
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
                set_url(flow, url, io);
            }
            LoginProgress::ShowCode { url, code } => {
                set_url(flow, url, io);
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
        let trusted = flow.url_trusted;
        url_rows = url_chunks(url, width, mode)
            .into_iter()
            .map(|chunk| {
                let mut row = RenderRow::plain(escape(&chunk), Role::Text);
                if trusted {
                    row.links.push(RenderLink {
                        range: 0..row.text.len(),
                        url: url.clone(),
                    });
                }
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
        let shown = flow
            .pasted
            .chars()
            .count()
            .min(width.saturating_sub(3).max(1));
        let mut input = RenderRow::plain(format!("> {}", "*".repeat(shown)), Role::Text);
        input.cursor = Some(2 + shown);
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
            if flow.url_trusted
                && let Some(url) = &flow.url
            {
                last.links.push(RenderLink {
                    range: 0..last.text.len(),
                    url: url.clone(),
                });
            }
        }
    }
    head.into_iter().chain(url_rows).chain(tail).collect()
}

/// Records the flow URL: only a validated http(s) URL reaches the desktop
/// opener or becomes a terminal hyperlink; anything else stays visible as
/// escaped text so the user can still read what the host sent.
fn set_url(flow: &mut FlowView, url: String, io: &dyn TermIo) {
    let trusted = crate::render::valid_http_url(&url);
    flow.opened = trusted && io.open_url(&url).is_ok();
    flow.url = Some(url);
    flow.url_trusted = trusted;
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
        .map(|def| PickerOption {
            label: def.id.to_owned(),
            action: PickerAction::Command(dal_core::Command::Run {
                name: "login".into(),
                args: def.id.into(),
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
    use std::sync::{Mutex, PoisonError};

    use super::*;

    /// Records every URL the desktop opener is asked to open.
    #[derive(Default)]
    struct OpenRecorder {
        opened: Mutex<Vec<String>>,
    }

    impl OpenRecorder {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.opened.lock().unwrap_or_else(PoisonError::into_inner))
        }
    }

    impl TermIo for OpenRecorder {
        fn enable_raw(&self) -> std::io::Result<()> {
            Ok(())
        }
        fn disable_raw(&self) {}
        fn size(&self) -> std::io::Result<(u16, u16)> {
            Ok((80, 24))
        }
        fn write(&self, _bytes: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn read(&self, _timeout: std::time::Duration) -> std::io::Result<Vec<u8>> {
            Ok(Vec::new())
        }
        fn raise_tstp(&self) {}
        fn open_url(&self, url: &str) -> std::io::Result<()> {
            self.opened
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(url.to_owned());
            Ok(())
        }
    }

    fn flow_sign_in() -> SignIn {
        SignIn {
            provider: "test".into(),
            stage: Stage::Flow(Box::default()),
            run: None,
        }
    }

    fn painted(sign_in: &SignIn) -> String {
        let theme = crate::theme::load(
            &crate::ThemeRequest::Palette,
            crate::ColorMode::Never,
            None,
            None,
        )
        .expect("palette theme loads");
        let mut out = Vec::new();
        for row in sign_in.rows(80, WidthMode::Narrow, 24) {
            crate::screen::driver::write_styled_text(
                &mut out, &row.text, &row.spans, &row.links, row.role, &theme,
            );
        }
        String::from_utf8(out).expect("terminal output is UTF-8")
    }

    #[test]
    fn pasted_authorization_is_masked_but_retained_for_exchange() {
        let pasted = "https://example.test/callback?code=secret-code&state=secret-state";
        let mut sign_in = SignIn {
            provider: "test".into(),
            stage: Stage::Flow(Box::new(FlowView {
                paste_hint: Some("Paste the redirect URL.".into()),
                ..FlowView::default()
            })),
            run: None,
        };
        sign_in.paste(pasted);

        let rows = sign_in.rows(80, WidthMode::Narrow, 24);
        let shown: String = rows.iter().map(|row| row.text.as_str()).collect();
        let mask = "*".repeat(pasted.chars().count());
        assert!(shown.contains(&format!("> {mask}")));
        assert!(!shown.contains("secret-code"));
        assert!(!shown.contains("secret-state"));
        let Stage::Flow(flow) = &sign_in.stage else {
            panic!("test flow must remain a flow");
        };
        assert_eq!(flow.pasted, pasted);
    }

    #[test]
    fn a_login_url_with_terminal_controls_opens_nothing_and_links_nothing() {
        let io = OpenRecorder::default();
        let mut sign_in = flow_sign_in();
        let payload = "https://example.test/login?\u{1b}]52;c;\u{7}\r\n".to_owned();
        sign_in.progress(LoginProgress::OpenUrl { url: payload }, &io);

        assert!(
            io.take().is_empty(),
            "a control-byte URL must not reach the opener"
        );
        let rows = sign_in.rows(80, WidthMode::Narrow, 24);
        let shown: String = rows.iter().map(|row| row.text.as_str()).collect();
        assert!(
            shown.contains("\\u{1b}"),
            "the payload stays visible: {shown}"
        );
        assert!(shown.contains("\\a"), "the payload stays visible: {shown}");
        assert!(shown.contains("\\r"), "the payload stays visible: {shown}");
        assert!(shown.contains("\\n"), "the payload stays visible: {shown}");
        for row in &rows {
            assert!(
                !row.text.chars().any(char::is_control),
                "no control survives: {:?}",
                row.text
            );
            assert!(
                row.links.is_empty(),
                "no link is built for an invalid URL: {:?}",
                row.text
            );
        }
    }

    #[test]
    fn login_rows_emit_no_terminal_controls_for_a_hostile_url() {
        let io = OpenRecorder::default();
        let mut sign_in = flow_sign_in();
        let payload = "https://example.test/x?\u{1b}]52;c;copied\u{7}\r\n".to_owned();
        sign_in.progress(LoginProgress::OpenUrl { url: payload }, &io);

        let bytes = painted(&sign_in);
        assert!(!bytes.contains('\u{7}'), "BEL never reaches the terminal");
        assert!(
            !bytes.contains("\u{1b}]"),
            "no OSC sequence opens from a payload: {bytes}"
        );
        assert!(bytes.contains("\\u{1b}"), "the payload is visible text");
    }

    #[test]
    fn an_https_login_url_opens_once_and_links_every_chunk() {
        let io = OpenRecorder::default();
        let mut sign_in = flow_sign_in();
        let url = "https://example.test/oauth/authorize?client_id=abc&state=xyz".to_owned();
        sign_in.progress(
            LoginProgress::ShowCode {
                url: url.clone(),
                code: "ABCD-1234".into(),
            },
            &io,
        );

        assert_eq!(io.take(), [url.as_str()]);
        let rows = sign_in.rows(80, WidthMode::Narrow, 24);
        let linked: Vec<&str> = rows
            .iter()
            .flat_map(|row| row.links.iter())
            .map(|link| link.url.as_str())
            .collect();
        assert!(!linked.is_empty(), "a valid URL stays clickable");
        assert!(linked.iter().all(|target| *target == url.as_str()));
        let shown: String = rows.iter().map(|row| row.text.as_str()).collect();
        assert!(shown.contains("https://example.test/oauth/authorize"));
    }

    #[test]
    fn login_rows_emit_one_clean_osc8_link_for_a_valid_url() {
        let io = OpenRecorder::default();
        let mut sign_in = flow_sign_in();
        let url = "https://example.test/oauth/authorize?client_id=abc".to_owned();
        sign_in.progress(LoginProgress::OpenUrl { url: url.clone() }, &io);

        let bytes = painted(&sign_in);
        assert_eq!(
            bytes.matches("\u{1b}]8;;https://example.test/").count(),
            1,
            "one validated OSC 8 target: {bytes}"
        );
        assert!(!bytes.contains('\u{7}'), "BEL never reaches the terminal");
    }

    #[test]
    fn a_non_http_login_url_is_shown_but_neither_opened_nor_linked() {
        let io = OpenRecorder::default();
        let mut sign_in = flow_sign_in();
        sign_in.progress(
            LoginProgress::OpenUrl {
                url: "file:///etc/passwd".into(),
            },
            &io,
        );

        assert_eq!(io.take(), [] as [String; 0]);
        let rows = sign_in.rows(80, WidthMode::Narrow, 24);
        let shown: String = rows.iter().map(|row| row.text.as_str()).collect();
        assert!(
            shown.contains("file:///etc/passwd"),
            "the URL stays visible for diagnosis: {shown}"
        );
        assert!(rows.iter().all(|row| row.links.is_empty()));
    }

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
