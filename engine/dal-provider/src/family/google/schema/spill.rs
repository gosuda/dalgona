//! Description spill: oh-my-pi `spill.ts` `spillToDescription` in its
//! `"spill"` format, the only one the Google spellings use.
//!
//! A keyword the wire schema drops is demoted into the node's `description`
//! so the model still reads the constraint. The value text is what
//! `JSON.stringify` prints, so numbers and strings match oh-my-pi byte for
//! byte.

use super::tree::{Fields, Node};

/// Appends `{key: json, ...}` for the stripped `entries` to the node's
/// `description`, after a blank line when one exists already.
pub(super) fn spill_to_description(node: &mut Fields, entries: &[(&str, &Node)]) {
    let formatted: Vec<String> = entries
        .iter()
        .filter(|(_, value)| !matches!(value, Node::Undefined))
        .map(|(key, value)| format!("{key}: {}", json_text(value)))
        .collect();
    if formatted.is_empty() {
        return;
    }
    let existing = node.text("description").unwrap_or("");
    let spilled = format!("{{{}}}", formatted.join(", "));
    let description = if existing.is_empty() {
        spilled
    } else {
        format!("{existing}\n\n{spilled}")
    };
    node.set("description", Node::Text(description));
}

/// `JSON.stringify(node)`: compact, with `undefined` members omitted.
fn json_text(node: &Node) -> String {
    let mut out = String::new();
    write_json(node, &mut out);
    out
}

/// Writes one value the way `JSON.stringify` does.
fn write_json(node: &Node, out: &mut String) {
    match node {
        Node::Undefined | Node::Null => out.push_str("null"),
        Node::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Node::Number(_) => write_number(node.number().unwrap_or(f64::NAN), out),
        Node::Text(text) => write_string(text, out),
        Node::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Node::Object(fields) => {
            out.push('{');
            let members = fields
                .iter()
                .filter(|(_, member)| !matches!(member, Node::Undefined));
            for (index, (name, member)) in members.enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(name, out);
                out.push(':');
                write_json(member, out);
            }
            out.push('}');
        }
    }
}

/// Writes a string literal with `JSON.stringify` escapes: the short forms for
/// `\b \f \n \r \t`, lowercase `\u00xx` for other control characters, and
/// everything else, including U+007F and U+2028, verbatim.
fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if control < ' ' => {
                let code = u32::from(control);
                out.push_str("\\u00");
                out.push(char::from_digit(code >> 4, 16).unwrap_or('0'));
                out.push(char::from_digit(code & 0xf, 16).unwrap_or('0'));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Writes a number the way `Number.prototype.toString` does (ECMAScript
/// `Number::toString`): shortest round-trip digits, positional notation for
/// decimal exponents in `-7 < n <= 21`, scientific otherwise. A non-finite
/// value prints as `null`, which is what `JSON.stringify` yields.
fn write_number(value: f64, out: &mut String) {
    if !value.is_finite() {
        out.push_str("null");
        return;
    }
    if value == 0.0 {
        out.push('0');
        return;
    }
    if value < 0.0 {
        out.push('-');
    }
    let scientific = format!("{:e}", value.abs());
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        out.push_str(&scientific);
        return;
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        out.push_str(&scientific);
        return;
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    // `value = 0.<digits> * 10^point`, the spec's `n`.
    let point = exponent + 1;
    write_digits(&digits, point, out);
}

/// Lays out `digits` with the decimal point `point` places in, per the
/// `Number::toString` cases.
fn write_digits(digits: &str, point: i32, out: &mut String) {
    let count = i32::try_from(digits.len()).unwrap_or(i32::MAX);
    if count <= point && point <= 21 {
        out.push_str(digits);
        out.push_str(&"0".repeat(usize::try_from(point - count).unwrap_or(0)));
        return;
    }
    if 0 < point && point <= 21 {
        let split = usize::try_from(point).unwrap_or(0);
        out.push_str(digits.get(..split).unwrap_or(digits));
        out.push('.');
        out.push_str(digits.get(split..).unwrap_or(""));
        return;
    }
    if -6 < point && point <= 0 {
        out.push_str("0.");
        out.push_str(&"0".repeat(usize::try_from(-point).unwrap_or(0)));
        out.push_str(digits);
        return;
    }
    let exponent = point - 1;
    out.push_str(digits.get(..1).unwrap_or(""));
    let rest = digits.get(1..).unwrap_or("");
    if !rest.is_empty() {
        out.push('.');
        out.push_str(rest);
    }
    out.push('e');
    out.push(if exponent < 0 { '-' } else { '+' });
    out.push_str(&exponent.unsigned_abs().to_string());
}
