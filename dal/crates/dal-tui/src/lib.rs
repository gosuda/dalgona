//! Terminal client for dal's replayable Host and Agent contracts.
//!
//! Rendering consumes sequenced [`dal_core::Update`] values and structured
//! [`dal_core::View`] snapshots. The interface never reads session journals.

#![deny(missing_docs)]

pub mod backend;
pub mod composer;
pub mod copy;
pub mod debug;
pub mod diagram;
pub mod dialog;
pub mod frame;
pub mod image;
pub mod keys;
pub mod live;
pub mod markdown;
pub mod picker;
pub mod popup;
pub mod screen;
pub mod status;
pub mod term;
pub mod theme;
pub mod transcript;
pub mod width;

use std::ffi::OsString;
use std::fmt;

use dal_agent::{AgentError, Host, HostError, SessionRef};
use dal_core::SessionId;
use thiserror::Error;
use tokio::runtime::Handle;

pub use term::{TermState, restore};
pub use width::WidthMode;

/// Selects the terminal surface used by the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Owns one live block while preserving terminal scrollback.
    Inline,
    /// Uses the alternate screen and replays the settled transcript on exit.
    Fullscreen,
}

/// Requests automatic, terminal-palette, or explicitly named colors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeRequest {
    /// Resolve from the terminal background probe, then `COLORFGBG`.
    Auto,
    /// Use the terminal's own palette without synthesized RGB colors.
    Palette,
    /// Resolve one of the nine shipped theme names.
    Named(Box<str>),
}

/// Color capability captured at the process edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Emit truecolor styles when a theme is resolved.
    Truecolor,
    /// Emit the nearest ANSI 256-color approximation.
    Ansi256,
    /// Use the terminal's 16-color palette.
    Sixteen,
    /// Emit no color SGR sequences.
    Never,
}

/// Environment facts captured once by the process edge.
#[derive(Debug, Clone, Default)]
pub struct EnvFacts {
    /// Whether standard input was a terminal at the process edge.
    pub stdin_tty: bool,
    /// The `PATH` value captured at the process edge, preserving non-Unicode values.
    pub path: Option<OsString>,
    /// The `TERM` value, when present.
    pub term: Option<String>,
    /// The `TERM_PROGRAM` value, when present.
    pub term_program: Option<String>,
    /// The `COLORTERM` value, when present.
    pub colorterm: Option<String>,
    /// The `COLORFGBG` value, when present.
    pub colorfgbg: Option<String>,
    /// The `WT_SESSION` value, when present.
    pub wt_session: Option<String>,
    /// The Windows Terminal version, when known.
    pub wt_version: Option<String>,
    /// Whether `TMUX` was present at the process edge.
    pub tmux: bool,
    /// Whether `STY` was present at the process edge.
    pub sty: bool,
    /// Whether `ZELLIJ` was present at the process edge.
    pub zellij: bool,
    /// Locale-derived Unicode width mode.
    pub width_mode: WidthMode,
    /// Whether reduced motion was requested at the process edge.
    pub no_motion: bool,
    /// Whether TUI diagnostics were requested at the process edge.
    pub debug: bool,
}

/// Options resolved by the process edge before terminal startup.
#[derive(Debug, Clone)]
pub struct TuiOptions {
    /// The explicit session and absolute workspace selected by the process edge.
    pub session: SessionRef,
    /// The selected terminal surface.
    pub screen: Screen,
    /// The theme selection.
    pub theme_request: ThemeRequest,
    /// Whether protocol-gated inline images are enabled.
    pub images: bool,
    /// Whether supported fenced diagrams are rendered in text surfaces.
    pub diagrams: bool,
    /// Whether repeated motion is enabled.
    pub motion: bool,
    /// The resolved external editor command.
    pub editor: Box<str>,
    /// The terminal color capability.
    pub color: ColorMode,
    /// Environment facts captured once by the process edge.
    pub env: EnvFacts,
    /// Runtime handle created by the process edge.
    pub rt: Handle,
}

/// Summary returned after the terminal has been restored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiExit {
    /// Session identity, when a persisted session was opened.
    pub session: Option<SessionId>,
    /// Display name of the session, when present.
    pub name: Option<Box<str>>,
    /// Number of transcript messages rendered.
    pub messages: u64,
    /// Whether the session was ephemeral.
    pub ephemeral: bool,
}

/// Errors returned by the terminal client.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TuiError {
    /// Terminal setup, keybinding, or rendering failure with its two-line user message.
    #[error("{0}")]
    Terminal(String),
    /// The host rejected a client operation.
    #[error("host error: {0}")]
    Host(#[from] HostError),
    /// A local agent rejected an operation.
    #[error("session error: {0}")]
    Agent(#[from] AgentError),
    /// A transport backend rejected an operation.
    #[error("remote session error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for Screen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Inline => "inline",
            Self::Fullscreen => "fullscreen",
        })
    }
}

/// Runs dal's terminal client over an existing host and process-edge terminal adapter.
///
/// The host remains the sole owner of sessions and provider operations. This function
/// opens one agent, snapshots its view, subscribes after that sequence, and restores
/// terminal modes before it waits for shutdown work. `io` supplies terminal operations
/// over handles captured by the process edge.
///
/// # Errors
/// Returns [`TuiError::Host`] when the host rejects session access, or
/// [`TuiError::Terminal`] when terminal setup or rendering fails.
pub fn run(host: Host, opts: TuiOptions, io: impl term::TermIo) -> Result<TuiExit, TuiError> {
    let model_host = host.clone();
    let rt = opts.rt.clone();
    run_backend(host, opts, io, move || {
        let models = rt.block_on(model_host.models(None))?;
        Ok(picker::model_options(models))
    })
}

/// Runs the terminal client against any implementation of the Host/Agent contract.
///
/// `models` supplies cached or remote model rows only when the user opens `/model`.
///
/// # Errors
/// Returns [`TuiError`] when the host, model source, session, or terminal operation fails.
#[must_use]
pub fn run_backend<H, M>(
    host: H,
    opts: TuiOptions,
    io: impl term::TermIo,
    models: M,
) -> Result<TuiExit, TuiError>
where
    H: backend::TuiHost,
    M: FnMut() -> Result<Vec<picker::ModelOption>, TuiError>,
{
    run_backend_with_settings_save(host, opts, io, models, |_| {
        Err(TuiError::Terminal(
            "dal-tui: this host cannot save product configuration".to_owned(),
        ))
    })
}

/// Runs the terminal client with a process-edge writer for product TUI settings.
///
/// # Errors
/// Returns [`TuiError`] when the host, model source, session, or terminal operation fails.
/// A settings-writer error is shown in the TUI and leaves the session-local
/// setting active.
#[must_use]
pub fn run_backend_with_settings_save<H, M, S>(
    host: H,
    opts: TuiOptions,
    io: impl term::TermIo,
    models: M,
    save_diagrams: S,
) -> Result<TuiExit, TuiError>
where
    H: backend::TuiHost,
    M: FnMut() -> Result<Vec<picker::ModelOption>, TuiError>,
    S: FnMut(bool) -> Result<(), TuiError>,
{
    runtime::run(host, opts, io, models, save_diagrams)
}

mod render;
mod runtime;

/// Draws one settled session frame into a ratatui test terminal.
///
/// The same pure row projection drives the live terminal loop; no host,
/// journal, wall clock, or terminal capability probe enters this function.
///
/// # Errors
/// Returns [`TuiError::Terminal`] when the test backend cannot draw a frame.
pub fn draw_frame(
    terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    screen: Screen,
    view: &dal_core::View,
    opts: &TuiOptions,
) -> Result<(), TuiError> {
    let mut transcript = transcript::Transcript::default();
    let diagram_settings = diagram::DiagramSettings {
        enabled: opts.diagrams,
    };
    let diagram_cache = diagram::RenderCache::with_path(opts.env.path.clone());
    let size = terminal
        .size()
        .map_err(|error| TuiError::Terminal(error.to_string()))?;
    for entry in &view.entries.items {
        let rows = render::entry_rows(
            entry,
            size.width,
            opts.env.width_mode,
            diagram_settings,
            &diagram_cache,
        );
        transcript.commit_rendered(&entry.id.to_string(), &rows);
    }
    let mut dialog = dialog::DialogUi::default();
    dialog.resync(view.open.clone());
    let live = live::Live::default();
    let theme = theme::load(
        &opts.theme_request,
        opts.color,
        None,
        opts.env.colorfgbg.as_deref(),
    )?;
    let rows = render::frame_rows(
        render::FrameInput {
            view,
            screen,
            composer: "",
            popup: &[],
            live: &live,
            dialog: &dialog,
            picker: None,
            transcript: &transcript,
            opts,
            theme: &theme,
            diagram_settings,
            diagram_cache: &diagram_cache,
        },
        size.width,
        size.height,
    );
    terminal
        .draw(|frame| {
            let height = rows.len().min(usize::from(size.height));
            let top = usize::from(size.height) - height;
            for (index, row) in rows.iter().take(height).enumerate() {
                let y = u16::try_from(top + index).unwrap_or(size.height);
                let paragraph = if row.spans.is_empty() {
                    ratatui::widgets::Paragraph::new(row.text.as_str())
                        .style(row_style(row.color, row.role))
                } else {
                    let spans = row
                        .spans
                        .iter()
                        .map(|span| {
                            ratatui::text::Span::styled(
                                &row.text[span.range.clone()],
                                row_style(theme.color(span.role), span.role),
                            )
                        })
                        .collect::<Vec<_>>();
                    ratatui::widgets::Paragraph::new(ratatui::text::Line::from(spans))
                };
                frame.render_widget(paragraph, ratatui::layout::Rect::new(0, y, size.width, 1));
            }
        })
        .map_err(|error| TuiError::Terminal(error.to_string()))?;
    Ok(())
}

fn row_style(color: ratatui::style::Color, role: theme::Role) -> ratatui::style::Style {
    ratatui::style::Style::new()
        .fg(color)
        .add_modifier(theme::ResolvedTheme::modifiers(role))
}

#[cfg(test)]
mod tests {
    use super::Screen;

    #[test]
    fn screen_labels_are_stable_for_edge_dispatch() {
        assert_eq!(Screen::Inline.to_string(), "inline");
        assert_eq!(Screen::Fullscreen.to_string(), "fullscreen");
    }
}
