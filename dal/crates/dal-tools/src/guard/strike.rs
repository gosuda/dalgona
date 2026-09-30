use sonic_rs::{JsonContainerTrait, JsonType, JsonValueTrait, Value};
use std::fmt::Write as _;

pub(super) fn call_key(tool: &str, args: &Value) -> [u8; 16] {
    let mut bytes = String::new();
    bytes.push('[');
    write_quoted(tool, &mut bytes);
    bytes.push(',');
    write_canonical(args, &mut bytes);
    bytes.push(']');
    let digest = blake3::hash(bytes.as_bytes());
    let mut key = [0; 16];
    key.copy_from_slice(&digest.as_bytes()[..16]);
    key
}

pub(super) fn display_key(tool: &str, key: &[u8; 16]) -> String {
    format!(
        "{tool} {:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        key[0], key[1], key[2], key[3], key[4], key[5]
    )
}

pub(super) fn canonical(value: &Value) -> String {
    let mut output = String::new();
    write_canonical(value, &mut output);
    output
}

fn write_canonical(value: &Value, output: &mut String) {
    match value.get_type() {
        JsonType::Null => output.push_str("null"),
        JsonType::Boolean => output.push_str(if value.as_bool().unwrap_or_default() {
            "true"
        } else {
            "false"
        }),
        JsonType::Number => write_number(value, output),
        JsonType::String => write_quoted(value.as_str().unwrap_or_default().trim(), output),
        JsonType::Array => {
            output.push('[');
            if let Some(items) = value.as_array() {
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    write_canonical(item, output);
                }
            }
            output.push(']');
        }
        JsonType::Object => {
            output.push('{');
            if let Some(object) = value.as_object() {
                let mut entries: Vec<_> = object.iter().collect();
                entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
                for (index, (key, item)) in entries.into_iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    write_quoted(key, output);
                    output.push(':');
                    write_canonical(item, output);
                }
            }
            output.push('}');
        }
    }
}

fn write_number(value: &Value, output: &mut String) {
    if let Some(number) = value.as_f64() {
        output.push_str(&round3(number).to_string());
    } else {
        output.push_str(&value.to_string());
    }
}

fn write_quoted(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                let _ = write!(output, "\\u{:04x}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

pub(super) fn round3(value: f64) -> f64 {
    if value == 0.0 {
        return 0.0;
    }
    if !value.is_finite() {
        return value;
    }
    let magnitude = value.abs().log10().floor();
    let scale = 10_f64.powf(magnitude);
    if scale == 0.0 || !scale.is_finite() {
        return value;
    }
    let significand = value / scale;
    let rounded = (significand * 100.0).round() / 100.0;
    let result = rounded * scale;
    if result.is_finite() && result != 0.0 {
        result
    } else {
        value
    }
}

#[derive(Default)]
pub(super) struct Stream {
    last: Option<[u8; 16]>,
    strikes: u8,
    last_errored: bool,
    last_sequence: Option<u128>,
}

impl Stream {
    pub(super) fn on_call(&mut self, key: [u8; 16], sequence: u128) -> u8 {
        if self.last == Some(key) {
            let increment = if self.last_errored { 2 } else { 1 };
            self.strikes = self.strikes.saturating_add(increment).min(3);
        } else {
            self.last = Some(key);
            self.strikes = 1;
        }
        self.last_sequence = Some(sequence);
        self.last_errored = false;
        self.strikes
    }

    pub(super) fn on_result(&mut self, key: [u8; 16], sequence: u128, ok: bool) {
        if self.last == Some(key) && self.last_sequence == Some(sequence) {
            self.last_errored = !ok;
        }
    }

    pub(super) fn strikes(&self) -> u8 {
        self.strikes
    }
}
