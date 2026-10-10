//! Shared journal decoding and folded-state helpers for `dalgon dev`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dal_core::{DeclaredFold, Record, Session, Timestamp, decode};

use super::DevError;

/// One journal's complete records and its torn tail, in store terms.
pub(super) struct JournalLines {
    /// Newline-terminated record lines, in file order.
    pub(super) records: Vec<Vec<u8>>,
    /// Bytes after the last newline: the tail the store quarantines as torn.
    pub(super) torn: usize,
}

/// Reads one journal file into its complete record lines.
///
/// Matches the store's durability grammar: a record exists only where a
/// newline terminates it, a blank record is `empty record` damage, and an
/// unterminated tail is torn — reported, never replayed.
///
/// # Errors
/// `Read` when the file cannot be read; `Decode` on a blank record.
pub(super) fn read_lines(path: &Path) -> Result<JournalLines, DevError> {
    let bytes = fs::read(path).map_err(|source| DevError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let mut records = Vec::new();
    let mut start = 0;
    for end in bytes
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
    {
        if end == start {
            return Err(DevError::Decode {
                path: path.display().to_string(),
                line: records.len() + 1,
                source: dal_core::DecodeError::Invalid {
                    offset: 0,
                    message: "empty record".into(),
                },
            });
        }
        records.push(bytes[start..end].to_vec());
        start = end + 1;
    }
    Ok(JournalLines {
        records,
        torn: bytes.len() - start,
    })
}

/// Decodes one journal line, reporting its 1-based position on failure.
pub(super) fn decode_line(path: &Path, line: &[u8], index: usize) -> Result<Record, DevError> {
    decode(line)
        .map(|decoded| decoded.record)
        .map_err(|source| DevError::Decode {
            path: path.display().to_string(),
            line: index + 1,
            source,
        })
}

/// Folds journal lines into the recovered session state.
pub(super) fn fold(path: &Path, lines: &[Vec<u8>]) -> Result<Session, DevError> {
    Session::replay_lines(lines.iter().map(Vec::as_slice), Timestamp::now())
        .map(|(session, _effects)| session)
        .map_err(|source| DevError::Replay {
            path: path.display().to_string(),
            source,
        })
}

/// One running declared fold for `dev fold`: pushes records one at a
/// time so attribution keeps a single fold instead of replaying every
/// prefix, while reporting replay failures against the journal path.
pub(super) struct DeclaredLines<'a> {
    fold: DeclaredFold,
    path: &'a Path,
}

impl<'a> DeclaredLines<'a> {
    /// An empty fold reporting errors against `path`.
    pub(super) fn new(path: &'a Path) -> Self {
        Self {
            fold: DeclaredFold::new(),
            path,
        }
    }

    /// Folds one decoded record and returns the session it declares.
    ///
    /// A borrow of the fold, not a clone: the per-record diff only reads
    /// the projected fields before the next push.
    ///
    /// # Errors
    /// `Replay` on a contradiction or unsupported version.
    pub(super) fn push(&mut self, record: &Record) -> Result<&Session, DevError> {
        self.fold.push(record).map_err(|source| DevError::Replay {
            path: self.path.display().to_string(),
            source,
        })?;
        self.fold.session().map_err(|source| DevError::Replay {
            path: self.path.display().to_string(),
            source,
        })
    }
}

/// Folds a journal file end to end.
pub(super) fn load(path: &Path) -> Result<(Vec<Vec<u8>>, usize, Session), DevError> {
    let lines = read_lines(path)?;
    if lines.records.is_empty() {
        return Err(DevError::Empty {
            path: path.display().to_string(),
        });
    }
    let session = fold(path, &lines.records)?;
    Ok((lines.records, lines.torn, session))
}

/// The wire kind of one journal record.
pub(super) fn record_kind(record: &Record) -> String {
    let debug = format!("{record:?}");
    let end = debug.find([' ', '(', '{']).unwrap_or(debug.len());
    debug[..end].to_owned()
}

/// Maps each top-level folded field to its pretty-printed subtree.
///
/// The session state keeps its fields private; the debug dump is the one
/// complete, field-labelled projection, so this parses its indentation back
/// into a name -> text map for field-level diffs.
pub(super) fn fields(session: &Session) -> BTreeMap<String, String> {
    let dump = format!("{session:#?}");
    let mut map = BTreeMap::new();
    let mut name: Option<String> = None;
    let mut body = String::new();
    let flush =
        |map: &mut BTreeMap<String, String>, name: &mut Option<String>, body: &mut String| {
            if let Some(key) = name.take() {
                map.insert(key, std::mem::take(body));
            }
        };
    for line in dump.lines() {
        let field = line
            .strip_prefix("    ")
            .filter(|rest| !rest.starts_with(' '))
            .and_then(|rest| rest.split_once(':'))
            .map(|(key, _)| key);
        match field {
            Some(key) => {
                flush(&mut map, &mut name, &mut body);
                name = Some(key.to_owned());
                line.clone_into(&mut body);
            }
            None => {
                body.push_str(line);
            }
        }
    }
    flush(&mut map, &mut name, &mut body);
    // Debug order for hash collections is unstable across processes:
    // sort every `{...}` group's items so equal state compares equal.
    // List (`[...]`) order is meaningful and stays untouched.
    map.into_iter()
        .map(|(name, body)| (name, sort_brace_groups(&body)))
        .collect()
}

/// Canonicalizes one field body: inside every `{...}` group the top-level
/// items are sorted, recursively. Single-line groups are left verbatim so
/// brace text inside string fields is not rewritten.
fn sort_brace_groups(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..=open]);
        let mut depth = 1_usize;
        let mut close = None;
        for (at, ch) in rest[open + 1..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + 1 + at);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else {
            out.push_str(&rest[open + 1..]);
            return out;
        };
        let inner = &rest[open + 1..close];
        if inner.contains('\n') {
            let mut items: Vec<String> = split_items(inner)
                .into_iter()
                .map(|item| sort_brace_groups(item.trim()))
                .filter(|item| !item.is_empty())
                .collect();
            items.sort_unstable();
            out.push_str(&items.join(",\n    "));
        } else {
            out.push_str(inner);
        }
        out.push('}');
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Splits a `{...}` group's contents at commas nested in no bracket.
fn split_items(inner: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0;
    for (at, ch) in inner.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(&inner[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    items.push(&inner[start..]);
    items
}

#[cfg(test)]
mod tests {
    use super::sort_brace_groups;

    /// Journal-diff output must be byte-deterministic: multi-line
    /// `{...}` groups sort their items so equal sessions diff
    /// identically regardless of fold insertion order. Sequences and
    /// single-line groups keep their order.
    #[test]
    fn brace_groups_sort_their_items() {
        assert_eq!(
            sort_brace_groups("jobs = {\nb = 1,\na = 2\n}"),
            "jobs = {a = 2,\n    b = 1}"
        );
        assert_eq!(sort_brace_groups("x = {b, a}"), "x = {b, a}");
        assert_eq!(
            sort_brace_groups("turns = [\nb,\na\n]"),
            "turns = [\nb,\na\n]"
        );
        assert_eq!(
            sort_brace_groups("x = {\nz = {\nb,\na\n},\na = 1\n}"),
            "x = {a = 1,\n    z = {a,\n    b}}"
        );
    }
}
