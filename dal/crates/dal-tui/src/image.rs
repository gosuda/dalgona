//! Image cards: rung resolution, placeholder text, kitty chunking.

use crate::EnvFacts;

/// Paint rung admitted for one environment and probe result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rung {
    /// Kitty graphics directly.
    KittyDirect,
    /// Kitty placeholders under a multiplexer.
    KittyPlaceholders,
    /// iTerm2 inline images (never paints image cards).
    ITerm2,
    /// Sixel graphics.
    Sixel,
}

/// Probe facts relevant to image rung resolution.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageProbe {
    /// Kitty graphics query succeeded.
    pub kitty_ok: bool,
    /// DA1 advertised sixel.
    pub da1_sixel: bool,
}

/// Resolves the rung once at startup; first match wins.
#[must_use]
pub fn resolve_rung(env: &EnvFacts, probe: ImageProbe, images: bool) -> Option<Rung> {
    if !images {
        return None;
    }
    let multiplexer = env.tmux || env.sty || env.zellij;
    if multiplexer {
        if kitty_marker(env) && probe.kitty_ok {
            return Some(Rung::KittyPlaceholders);
        }
        return None;
    }
    if kitty_marker(env) || probe.kitty_ok {
        return Some(Rung::KittyDirect);
    }
    if matches!(env.term_program.as_deref(), Some("iTerm.app" | "WezTerm")) {
        return Some(Rung::ITerm2);
    }
    if probe.da1_sixel
        && (env.term.as_deref().is_some_and(|term| {
            term.starts_with("foot")
                || term.starts_with("mlterm")
                || term.starts_with("xterm")
                || term.starts_with("vt340")
        }) || windows_terminal_gate(env))
    {
        return Some(Rung::Sixel);
    }
    None
}

fn kitty_marker(env: &EnvFacts) -> bool {
    matches!(env.term_program.as_deref(), Some("kitty" | "ghostty"))
        || matches!(env.term.as_deref(), Some("xterm-kitty" | "xterm-ghostty"))
}

fn windows_terminal_gate(env: &EnvFacts) -> bool {
    let Some(session) = env.wt_session.as_deref() else {
        return false;
    };
    if session.is_empty() {
        return false;
    }
    let Some(version) = env.wt_version.as_deref() else {
        return false;
    };
    let mut parts = version.split('.');
    let major: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
    (major, minor) >= (1, 22)
}

/// Formats an image placeholder card.
#[must_use]
pub fn card(name: &str, dimensions: Option<(u32, u32)>, bytes: usize) -> String {
    let size = if bytes < 1_024 {
        format!("{bytes} B")
    } else if bytes < 1_048_576 {
        let tenths = (bytes * 10 + 512) / 1_024;
        format!("{}.{} KiB", tenths / 10, tenths % 10)
    } else {
        let tenths = (bytes * 10 + 524_288) / 1_048_576;
        format!("{}.{} MiB", tenths / 10, tenths % 10)
    };
    let (width, height) = dimensions.map_or(("?".to_owned(), "?".to_owned()), |(width, height)| {
        (width.to_string(), height.to_string())
    });
    crate::copy::render(
        crate::copy::ids::IMAGE_CARD,
        &[
            ("name", name),
            ("w", &width),
            ("h", &height),
            ("size", &size),
        ],
        1,
    )
}

/// Splits PNG bytes into kitty transmission chunks of exactly 3072 bytes.
#[must_use]
pub fn kitty_chunks(png: &[u8]) -> Vec<(bool, Vec<u8>)> {
    png.chunks(3_072)
        .enumerate()
        .map(|(index, chunk)| (index + 1 < png.chunks(3_072).len(), chunk.to_vec()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{ImageProbe, Rung, card, kitty_chunks, resolve_rung};
    use crate::EnvFacts;

    fn env() -> EnvFacts {
        EnvFacts::default()
    }

    #[test]
    fn images_off_never_admits_a_rung() {
        assert_eq!(
            resolve_rung(
                &env(),
                ImageProbe {
                    kitty_ok: true,
                    da1_sixel: true
                },
                false
            ),
            None
        );
    }

    #[test]
    fn rung_matrix_matches_the_contract() {
        let mut kitty = env();
        kitty.term = Some("xterm-kitty".to_owned());
        assert_eq!(
            resolve_rung(&kitty, ImageProbe::default(), true),
            Some(Rung::KittyDirect)
        );
        let mut mux = env();
        mux.tmux = true;
        mux.term = Some("xterm-kitty".to_owned());
        assert_eq!(
            resolve_rung(
                &mux,
                ImageProbe {
                    kitty_ok: true,
                    da1_sixel: false
                },
                true
            ),
            Some(Rung::KittyPlaceholders)
        );
        let mut foot = env();
        foot.term = Some("foot".to_owned());
        assert_eq!(
            resolve_rung(
                &foot,
                ImageProbe {
                    kitty_ok: false,
                    da1_sixel: true
                },
                true
            ),
            Some(Rung::Sixel)
        );
        assert_eq!(resolve_rung(&foot, ImageProbe::default(), true), None);
    }

    #[test]
    fn placeholder_renders_without_protocol_bytes() {
        assert_eq!(
            card("a.png", Some((64, 64)), 2_150),
            "image a.png · 64x64 · 2.1 KiB"
        );
        assert!(!card("a.png", Some((64, 64)), 2_150).contains('\x1b'));
    }

    #[test]
    fn kitty_chunks_split_at_3072() {
        let png = vec![0_u8; 5_000];
        let chunks = kitty_chunks(&png);
        assert_eq!(chunks.len(), 2);
        assert_eq!((chunks[0].0, chunks[0].1.len()), (true, 3_072));
        assert_eq!((chunks[1].0, chunks[1].1.len()), (false, 1_928));
    }
}
