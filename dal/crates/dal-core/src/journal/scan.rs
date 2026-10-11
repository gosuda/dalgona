use std::borrow::Cow;

use super::decode::invalid;
use crate::journal::DecodeError;

/// One scanned object member: borrowed name and value text plus the absolute
/// byte offset at which the value starts.
pub(super) struct Member<'a> {
    pub(super) name: Cow<'a, str>,
    pub(super) value: &'a str,
    pub(super) offset: usize,
}

/// The scanned members of one object, in document order.
pub(super) type Members<'a> = Vec<Member<'a>>;

/// Scans one JSON object into borrowed members.
///
/// Structure walking stays on sonic's iterators (SIMD); only the member map
/// changes shape: document order in a vector, no per-member heap names, and
/// values stay raw slices for single typed parsing by the caller. Offsets
/// and error shapes match the previous DOM walk exactly.
pub(super) fn scan_members(raw: &str, base: usize) -> Result<Members<'_>, DecodeError> {
    let mut members = Vec::new();
    for member in sonic_rs::to_object_iter(raw) {
        let (name, value) = member.map_err(|error| invalid(base, error.to_string()))?;
        let slice = value.as_raw_str();
        let start = slice.as_ptr() as usize - raw.as_ptr() as usize;
        let value = raw
            .get(start..start + slice.len())
            .ok_or_else(|| invalid(base, "malformed JSON"))?;
        let offset = base + start;
        members.push(Member {
            name,
            value,
            offset,
        });
    }
    Ok(members)
}

/// Scans one JSON array into item slices with absolute value offsets.
pub(super) fn scan_items(raw: &str, base: usize) -> Result<Vec<(usize, &str)>, DecodeError> {
    let mut items = Vec::new();
    for item in sonic_rs::to_array_iter(raw) {
        let item = item.map_err(|error| invalid(base, error.to_string()))?;
        let slice = item.as_raw_str();
        let start = slice.as_ptr() as usize - raw.as_ptr() as usize;
        let value = raw
            .get(start..start + slice.len())
            .ok_or_else(|| invalid(base, "malformed JSON"))?;
        let offset = base + start;
        items.push((offset, value));
    }
    Ok(items)
}
