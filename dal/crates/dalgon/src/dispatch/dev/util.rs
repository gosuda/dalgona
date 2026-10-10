//! Shared journal decoding and folded-state helpers for `dalgon dev`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dal_core::{Record, Session, Timestamp, decode};

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

/// Folds journal lines into the session they declare, without replay's
/// crash-recovery synthesis: `dev fold` attributes each record's state
/// change, so an open turn must stay open.
pub(super) fn fold_declared(path: &Path, lines: &[Vec<u8>]) -> Result<Session, DevError> {
    let mut records = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        records.push(decode_line(path, line, index)?);
    }
    Session::replay_declared(records).map_err(|source| DevError::Replay {
        path: path.display().to_string(),
        source,
    })
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
    map
}
