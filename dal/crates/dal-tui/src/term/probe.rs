//! Terminal capability probe parsers and incremental reply handling.

/// Terminal capabilities learned from the startup probe.
#[derive(Debug, Clone, Default, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent advertised capabilities; a bitset loses legibility"
)]
pub struct Probe {
    /// DECRPM 2026 synchronized updates are supported.
    pub sync_update: bool,
    /// DECRPM 2027 grapheme-cluster mode is supported.
    pub grapheme_mode: bool,
    /// Kitty keyboard protocol is supported.
    pub kitty_keyboard: bool,
    /// Kitty graphics query succeeded.
    pub kitty_graphics: bool,
    /// DA1 advertised sixel.
    pub sixel: bool,
    /// A DA1 reply arrived, ending the probe window early.
    pub da1: bool,
    /// OSC 11 background luminance when a valid reply arrived.
    pub background_luminance: Option<f64>,
}

/// Parses a DECRPM response for a terminal mode.
#[must_use]
pub fn parse_decrpm(bytes: &[u8], mode: u16) -> bool {
    decrpm_status(bytes, mode).is_some_and(|status| matches!(status, 1..=3))
}

fn decrpm_status(bytes: &[u8], mode: u16) -> Option<u8> {
    let reply = std::str::from_utf8(bytes).ok()?;
    let parameters = reply.strip_prefix("\x1b[?")?.strip_suffix("$y")?;
    let (reported_mode, status) = parameters.split_once(';')?;
    if reported_mode.parse::<u16>().ok()? != mode {
        return None;
    }
    status.parse::<u8>().ok()
}

fn is_decrpm_reply(bytes: &[u8]) -> bool {
    let Ok(reply) = std::str::from_utf8(bytes) else {
        return false;
    };
    reply.starts_with("\x1b[?") && reply.ends_with("$y")
}

/// Parses the kitty keyboard-protocol query response.
#[must_use]
pub fn parse_kitty_keyboard(bytes: &[u8]) -> bool {
    let Ok(reply) = std::str::from_utf8(bytes) else {
        return false;
    };
    reply.starts_with("\x1b[?") && reply.ends_with('u')
}

/// Parses a DA1 response and returns whether its parameter list advertises sixel.
#[must_use]
pub fn parse_da1(bytes: &[u8]) -> bool {
    let Ok(reply) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some(parameters) = reply.strip_prefix("\x1b[?") else {
        return false;
    };
    let Some(parameters) = parameters.strip_suffix('c') else {
        return false;
    };
    parameters
        .split(';')
        .any(|parameter| parameter.parse::<u16>().ok() == Some(4))
}

/// Parses an OSC 11 reply into relative background luminance.
#[must_use]
pub fn parse_osc11(bytes: &[u8]) -> Option<f64> {
    let reply = std::str::from_utf8(bytes).ok()?;
    let body = reply.strip_prefix("\x1b]11;")?;
    let body = body
        .strip_suffix('\u{7}')
        .or_else(|| body.strip_suffix("\x1b\\"))?;
    let rgb = body
        .strip_prefix("rgb:")
        .and_then(parse_rgb_slash)
        .or_else(|| parse_rgb_hash(body))?;
    let [red, green, blue] = rgb;
    Some(0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue))
}

fn parse_rgb_slash(value: &str) -> Option<[f64; 3]> {
    let mut channels = value.split('/');
    let red = parse_hex_channel(channels.next()?)?;
    let green = parse_hex_channel(channels.next()?)?;
    let blue = parse_hex_channel(channels.next()?)?;
    channels.next().is_none().then_some([red, green, blue])
}

fn parse_rgb_hash(value: &str) -> Option<[f64; 3]> {
    let hex = value.strip_prefix('#')?;
    let channel_digits = match hex.len() {
        6 => 2,
        12 => 4,
        _ => return None,
    };
    let mut channels = [0.0; 3];
    for (index, channel) in channels.iter_mut().enumerate() {
        let start = index * channel_digits;
        *channel = parse_hex_channel(&hex[start..start + channel_digits])?;
    }
    Some(channels)
}

fn parse_hex_channel(value: &str) -> Option<f64> {
    if !(1..=4).contains(&value.len()) {
        return None;
    }
    let number = u32::from_str_radix(value, 16).ok()?;
    let max = (1_u32 << (value.len() * 4)) - 1;
    Some(f64::from(number) / f64::from(max))
}

fn linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// Incremental probe parser that keeps incomplete terminal replies between reads.
#[derive(Debug, Default)]
pub struct ReplyParser {
    pending: Vec<u8>,
}

impl ReplyParser {
    /// Parses complete replies from `bytes`, returning ordinary key bytes for replay.
    pub fn feed(&mut self, bytes: &[u8]) -> (Probe, Vec<u8>) {
        self.pending.extend_from_slice(bytes);
        let mut probe = Probe::default();
        let mut replay = Vec::new();
        let mut cursor = 0;

        while cursor < self.pending.len() {
            if self.pending[cursor] != 0x1b {
                replay.push(self.pending[cursor]);
                cursor += 1;
                continue;
            }
            if cursor + 1 == self.pending.len() {
                break;
            }

            let start = cursor;
            match self.pending[cursor + 1] {
                b'[' => {
                    let Some(end) = find_csi_end(&self.pending, cursor + 2) else {
                        break;
                    };
                    let packet = &self.pending[start..=end];
                    if is_decrpm_reply(packet) {
                        probe.sync_update |= decrpm_status(packet, 2026)
                            .is_some_and(|status| matches!(status, 1..=3));
                        probe.grapheme_mode |= decrpm_status(packet, 2027)
                            .is_some_and(|status| matches!(status, 1..=3));
                    } else if parse_kitty_keyboard(packet) {
                        probe.kitty_keyboard = true;
                    } else if is_da1_reply(packet) {
                        probe.da1 = true;
                        probe.sixel |= parse_da1(packet);
                    } else {
                        replay.extend_from_slice(packet);
                    }
                    cursor = end + 1;
                }
                b']' => {
                    let Some(end) = find_osc_end(&self.pending, cursor + 2) else {
                        break;
                    };
                    let packet = &self.pending[start..end];
                    if packet.starts_with(b"\x1b]11;") {
                        probe.background_luminance = parse_osc11(packet);
                    } else {
                        replay.extend_from_slice(packet);
                    }
                    cursor = end;
                }
                b'_' => {
                    let Some(end) = find_st_end(&self.pending, cursor + 2) else {
                        break;
                    };
                    let packet = &self.pending[start..end + 2];
                    if packet.starts_with(b"\x1b_Gi=31;") {
                        probe.kitty_graphics |= packet.windows(2).any(|pair| pair == b"OK");
                    } else {
                        replay.extend_from_slice(packet);
                    }
                    cursor = end + 2;
                }
                _ => {
                    replay.push(self.pending[cursor]);
                    cursor += 1;
                }
            }
        }

        self.pending.drain(..cursor);
        (probe, replay)
    }

    /// Replays any incomplete bytes when the probe deadline has elapsed.
    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }
}

/// Parses complete replies from one byte slice and preserves bytes outside replies.
#[must_use]
pub fn parse_replies(bytes: &[u8]) -> (Probe, Vec<u8>) {
    let mut parser = ReplyParser::default();
    let (probe, mut replay) = parser.feed(bytes);
    replay.extend(parser.finish());
    (probe, replay)
}

fn is_da1_reply(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b[?") && bytes.ends_with(b"c")
}

fn find_csi_end(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..]
        .iter()
        .position(|byte| matches!(*byte, b'@'..=b'~'))
        .map(|offset| start + offset)
}

fn find_osc_end(bytes: &[u8], start: usize) -> Option<usize> {
    for index in start..bytes.len() {
        if bytes[index] == 0x07 {
            return Some(index + 1);
        }
        if bytes[index..].starts_with(b"\x1b\\") {
            return Some(index + 2);
        }
    }
    None
}

fn find_st_end(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start..]
        .windows(2)
        .position(|pair| pair == b"\x1b\\")
        .map(|offset| start + offset)
}

/// Builds the startup probe; graphics capabilities are queried only when images are enabled.
#[must_use]
pub fn startup_probe(images: bool) -> Vec<u8> {
    let mut probe = b"\x1b[?2026$p\x1b[?2027$p\x1b[?u\x1b]11;?\x07".to_vec();
    if images {
        probe.extend_from_slice(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    }
    probe.extend_from_slice(b"\x1b[c");
    probe
}
