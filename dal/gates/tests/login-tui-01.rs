//! Drives `/login` and `/logout` through the real terminal loop and the real
//! Host against a loopback OAuth server; asserts the rendered rows and the
//! credential file.

#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{
    error::Error,
    io,
    sync::{Arc, Mutex, PoisonError, mpsc},
    time::{Duration, Instant},
};

use dal_agent::login_fake::{FakeOAuth, TokenReply, follow_authorize_url};
use dal_agent::{Env, Host, SessionRef};
use dal_core::{Config, ConfigProduct, Workspace};
use dal_provider::{AuthStore, Credential, OAuthCredential, SecretString};
use dal_tui::term::TermIo;
use dal_tui::{ColorMode, EnvFacts, Screen, ThemeRequest, TuiOptions, WidthMode};
use support::TestDir;

type Fallible<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// A terminal whose input a test types and whose output it reads back.
struct TestTerminal {
    input: Mutex<mpsc::Receiver<Vec<u8>>>,
    output: Arc<Mutex<Vec<u8>>>,
    rt: tokio::runtime::Handle,
    /// Plays the browser: follows the authorize URL's loopback redirect.
    follow: bool,
    opened: Arc<Mutex<Vec<String>>>,
}

impl TermIo for TestTerminal {
    fn enable_raw(&self) -> io::Result<()> {
        Ok(())
    }
    fn disable_raw(&self) {}
    fn size(&self) -> io::Result<(u16, u16)> {
        Ok((80, 24))
    }
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.output
            .lock()
            .map_err(|_| io::Error::other("output lock"))?
            .extend_from_slice(bytes);
        Ok(())
    }
    fn read(&self, timeout: Duration) -> io::Result<Vec<u8>> {
        let input = self
            .input
            .lock()
            .map_err(|_| io::Error::other("input lock"))?;
        Ok(input.recv_timeout(timeout).unwrap_or_default())
    }
    fn raise_tstp(&self) {}
    fn open_url(&self, url: &str) -> io::Result<()> {
        self.opened
            .lock()
            .map_err(|_| io::Error::other("opened lock"))?
            .push(url.to_owned());
        if self.follow {
            let url = url.to_owned();
            let rt = self.rt.clone();
            std::thread::spawn(move || {
                let _page = rt.block_on(follow_authorize_url(&url, "auth-code"));
            });
        }
        Ok(())
    }
}

/// Removes terminal control sequences so rows can be searched as text.
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            out.push(character);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                for next in chars.by_ref() {
                    if next == '\u{7}' || next == '\\' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

struct Rig {
    rt: tokio::runtime::Runtime,
    data: TestDir,
    workspace: TestDir,
    fake: FakeOAuth,
    host: Host,
}

fn rig() -> Fallible<Rig> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let data = TestDir::new()?;
    let workspace = TestDir::new()?;
    let factory = dalgon::product();
    let config = Config::load(ConfigProduct::Dalgon, data.path(), factory.defaults, None)?;
    let product = (factory.build)(&dalgon::BuildCx {
        data_root: data.path().to_path_buf(),
        config: &config,
    })?;
    let env = Env {
        vars: support::captured_shell_vars(),
        cwd: workspace.path().to_path_buf(),
        sandbox_helper: None,
    };
    let fake = rt.block_on(FakeOAuth::start(TokenReply::Issue))?;
    let host = rt.block_on(Host::start(product, config, env))?;
    host.set_login_endpoints(fake.endpoints(0, 0)?)?;
    Ok(Rig {
        rt,
        data,
        workspace,
        fake,
        host,
    })
}

/// A running terminal loop and the handles a test drives it with.
struct Session {
    keys: mpsc::Sender<Vec<u8>>,
    output: Arc<Mutex<Vec<u8>>>,
    opened: Arc<Mutex<Vec<String>>>,
    thread: std::thread::JoinHandle<Result<(), String>>,
}

impl Session {
    fn start(rig: &Rig, follow: bool) -> Fallible<Self> {
        let (keys, input) = mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let opened = Arc::new(Mutex::new(Vec::new()));
        let terminal = TestTerminal {
            input: Mutex::new(input),
            output: Arc::clone(&output),
            rt: rig.rt.handle().clone(),
            follow,
            opened: Arc::clone(&opened),
        };
        let options = TuiOptions {
            session: SessionRef::Ephemeral {
                workspace: Workspace::new(rig.workspace.path().to_path_buf())?,
            },
            screen: Screen::Inline,
            theme_request: ThemeRequest::Palette,
            default_model: None,
            images: false,
            diagrams: false,
            motion: false,
            editor: "vi".into(),
            color: ColorMode::Never,
            binary: "dalgon",
            env: EnvFacts {
                stdin_tty: true,
                term: Some("xterm-256color".to_owned()),
                width_mode: WidthMode::Narrow,
                ..EnvFacts::default()
            },
            rt: rig.rt.handle().clone(),
        };
        let host = rig.host.clone();
        let rt = rig.rt.handle().clone();
        let thread = std::thread::spawn(move || {
            let model_host = host.clone();
            let models = move || {
                let rows = rt.block_on(model_host.models(None))?;
                Ok(dal_tui::picker::model_options(rows))
            };
            dal_tui::run_backend(&host, &options, &terminal, models)
                .map(|_| ())
                .map_err(|error| error.to_string())
        });
        Ok(Self {
            keys,
            output,
            opened,
            thread,
        })
    }

    fn type_text(&self, text: &str) {
        // A finished loop has no reader left; a send then has nothing to do.
        let _ = self.keys.send(text.as_bytes().to_vec());
    }

    fn screen(&self) -> String {
        plain(&self.output.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Waits until `needle` has been rendered after byte offset `from`.
    fn wait_for(&self, needle: &str, from: usize) -> Fallible<usize> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let screen = self.screen();
            if let Some(tail) = screen.get(from..)
                && tail.contains(needle)
            {
                return Ok(screen.len());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.type_text("\u{4}");
        Err(format!(
            "timed out waiting for {needle:?}; screen:\n{}",
            self.screen()
        )
        .into())
    }

    fn finish(self) -> Fallible<()> {
        self.type_text("\u{1b}");
        std::thread::sleep(Duration::from_millis(100));
        self.type_text("\u{4}");
        self.thread
            .join()
            .map_err(|_| "terminal loop panicked")?
            .map_err(Into::into)
    }
}

#[test]
fn login_openai_codex_completes_on_the_browser_callback_then_logout_removes_it() -> Fallible<()> {
    let rig = rig()?;
    let session = Session::start(&rig, true)?;
    let mark = session.wait_for("Ask dal to change code", 0)?;

    session.type_text("/login openai-codex\r");
    let mark = session.wait_for("Signing in to openai-codex", mark)?;
    let mark = session.wait_for("Open this URL in a browser:", mark)?;
    let mark = session.wait_for("Pick a model", mark)?;
    assert!(
        !session
            .screen()
            .contains("not available in the terminal client"),
        "the login picker notice no longer fires"
    );
    let opened = session.opened.lock().expect("opened lock").clone();
    assert_eq!(opened.len(), 1, "the browser was asked once");
    assert!(opened[0].contains("/oauth/authorize"), "{opened:?}");

    let store = AuthStore::load(rig.data.path().join("auth.json"))?;
    assert!(matches!(
        store.credential("openai-codex"),
        Some(Credential::OAuth(oauth)) if oauth.account_id.as_deref() == Some("acct-fake")
    ));

    session.type_text("\u{1b}");
    session.type_text("/logout\r");
    session.wait_for("openai-codex · oauth", mark)?;
    let mark = session.wait_for("All providers", mark)?;
    session.type_text("\r");
    session.wait_for("Removed credentials for openai-codex.", mark)?;
    assert!(!rig.data.path().join("auth.json").exists());
    assert_eq!(rig.fake.requests_to("/oauth/revoke").len(), 1);

    session.finish()
}

#[test]
fn login_anthropic_accepts_a_pasted_code() -> Fallible<()> {
    let rig = rig()?;
    let session = Session::start(&rig, false)?;
    let mark = session.wait_for("Ask dal to change code", 0)?;

    session.type_text("/login anthropic\r");
    let mark = session.wait_for(
        "Paste the redirect URL or the code shown in the browser.",
        mark,
    )?;
    session.type_text("pasted-code\r");
    session.wait_for("Pick a model", mark)?;

    let store = AuthStore::load(rig.data.path().join("auth.json"))?;
    assert!(matches!(
        store.credential("anthropic"),
        Some(Credential::OAuth(_))
    ));
    session.finish()
}

#[test]
fn escape_cancels_a_pending_login_and_stores_nothing() -> Fallible<()> {
    let rig = rig()?;
    let session = Session::start(&rig, false)?;
    let mark = session.wait_for("Ask dal to change code", 0)?;

    session.type_text("/login openai-codex\r");
    let mark = session.wait_for("Waiting for sign-in...", mark)?;
    session.type_text("\u{1b}");
    session.wait_for("sign-in cancelled.", mark)?;
    assert!(!rig.data.path().join("auth.json").exists());
    assert_eq!(
        rig.fake.requests_to("/oauth/token"),
        [] as [dal_agent::login_fake::Recorded; 0]
    );
    session.finish()
}

#[test]
fn login_openai_masks_the_key_and_logout_all_asks_first() -> Fallible<()> {
    let rig = rig()?;
    let session = Session::start(&rig, false)?;
    let mark = session.wait_for("Ask dal to change code", 0)?;

    session.type_text("/login openai\r");
    let mark = session.wait_for("Paste your openai API key", mark)?;
    session.type_text("sk-secret-value");
    let mark = session.wait_for("> ***************", mark)?;
    assert!(
        !session.screen().contains("sk-secret-value"),
        "the key is masked"
    );
    session.type_text("\r");
    let mark = session.wait_for("Pick a model", mark)?;
    let store = AuthStore::load(rig.data.path().join("auth.json"))?;
    assert!(matches!(
        store.credential("openai"),
        Some(Credential::ApiKey { key }) if key.expose() == "sk-secret-value"
    ));

    let mut seeded = store;
    seeded.set(
        "anthropic",
        Credential::OAuth(OAuthCredential {
            access_token: SecretString::from("a"),
            refresh_token: SecretString::from("r"),
            expires_at: None,
            id_token: None,
            account_id: None,
        }),
    )?;
    seeded.store()?;

    session.type_text("\u{1b}");
    session.type_text("/logout\r");
    let mark = session.wait_for("openai · api_key", mark)?;
    // Rows: anthropic, openai, then All providers; filter to the last row.
    session.type_text("All");
    session.type_text("\r");
    let mark = session.wait_for("Remove all credentials for every provider.", mark)?;
    session.type_text("n");
    assert!(
        rig.data.path().join("auth.json").exists(),
        "Keep them keeps both"
    );
    session.type_text("/logout\r");
    session.wait_for("All providers", mark)?;
    session.type_text("All");
    session.type_text("\r");
    session.type_text("y");
    session.wait_for("Removed all credentials.", 0)?;
    assert!(!rig.data.path().join("auth.json").exists());
    session.finish()
}
