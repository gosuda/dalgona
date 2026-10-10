//! Shared journal decoding and folded-state helpers for `dalgon dev`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use dal_core::{Record, Session, Timestamp, decode};

use super::DevError;

/// Reads one journal file into its non-empty record lines.
pub(super) fn read_lines(path: &Path) -> Result<Vec<Vec<u8>>, DevError> {
    let bytes = fs::read(path).map_err(|source| DevError::Read {
        path: path.display().to_string(),
        source,
    })?;
    Ok(bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
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

/// Folds a journal file end to end.
pub(super) fn load(path: &Path) -> Result<(Vec<Vec<u8>>, Session), DevError> {
    let lines = read_lines(path)?;
    if lines.is_empty() {
        return Err(DevError::Empty {
            path: path.display().to_string(),
        });
    }
    let session = fold(path, &lines)?;
    Ok((lines, session))
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
