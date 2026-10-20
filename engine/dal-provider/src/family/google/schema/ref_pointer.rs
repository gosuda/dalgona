//! Local `$ref` URI fragments as RFC 6901 JSON Pointers: oh-my-pi
//! `omptype/src/local-ref.ts` `parseLocalRefPointer` and
//! `formatLocalRefPointer`.
//!
//! A fragment is percent-decoded with `decodeURIComponent` semantics (every
//! escape decodes, the decoded bytes must be UTF-8), split on `/`, and each
//! token has `~1` and `~0` unescaped, in that order.

/// Parses a local reference into its decoded pointer tokens: `[]` for the
/// root `#`; `None` for a non-local reference, a fragment that does not
/// percent-decode, or one that does not start with `/`.
pub(super) fn parse_local_ref_pointer(reference: &str) -> Option<Vec<String>> {
    let fragment = reference.strip_prefix('#')?;
    let pointer = percent_decode(fragment)?;
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return None;
    }
    Some(
        pointer
            .split('/')
            .skip(1)
            .map(|token| token.replace("~1", "/").replace("~0", "~"))
            .collect(),
    )
}

/// Encodes pointer tokens back into a local reference: the inverse of
/// [`parse_local_ref_pointer`]. `$` stays readable; `%` stays encoded.
pub(super) fn format_local_ref_pointer(tokens: &[String]) -> String {
    if tokens.is_empty() {
        return "#".to_owned();
    }
    let encoded: Vec<String> = tokens
        .iter()
        .map(|token| encode_uri_component(&token.replace('~', "~0").replace('/', "~1")))
        .map(|token| token.replace("%24", "$"))
        .collect();
    format!("#/{}", encoded.join("/"))
}

/// `decodeURIComponent`: `None` for a malformed escape or non-UTF-8 result.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(byte);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// The value of one ASCII hex digit.
fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

/// `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` is
/// percent-encoded as uppercase UTF-8 bytes.
fn encode_uri_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for &byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            encoded.push(char::from(byte));
            continue;
        }
        encoded.push('%');
        for nibble in [byte >> 4, byte & 0xf] {
            let digit = char::from_digit(u32::from(nibble), 16).unwrap_or('0');
            encoded.push(digit.to_ascii_uppercase());
        }
    }
    encoded
}
