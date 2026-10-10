//! `dalgon dev fold`: per-record fold attribution over one journal.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use super::DevError;
use super::util;
use crate::exit;

/// Prints each journal line with the folded fields it changed.
///
/// Each record replays the journal prefix, so cost grows quadratically with
/// line count; journals hold hundreds of records, not millions.
pub(super) fn run(path: &Path) -> Result<ExitCode, DevError> {
    let lines = util::read_lines(path)?;
    if lines.records.is_empty() {
        return Err(DevError::Empty {
            path: path.display().to_string(),
        });
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if lines.torn > 0 {
        let _ = writeln!(out, "torn tail: {} bytes ignored", lines.torn);
    }
    let mut prior: BTreeMap<String, String> = BTreeMap::new();
    for (index, line) in lines.records.iter().enumerate() {
        let record = util::decode_line(path, line, index)?;
        let session = util::fold_declared(path, &lines.records[..=index])?;
        let fields = util::fields(&session);
        let mut delta = String::new();
        for (name, value) in &fields {
            match prior.get(name) {
                None => {
                    delta.push_str(" +");
                    delta.push_str(name);
                }
                Some(old) if old != value => {
                    delta.push_str(" ~");
                    delta.push_str(name);
                }
                Some(_) => {}
            }
        }
        for name in prior.keys().filter(|name| !fields.contains_key(*name)) {
            delta.push_str(" -");
            delta.push_str(name);
        }
        let _ = writeln!(
            out,
            "{:>4}  {:<18}{delta}",
            index + 1,
            util::record_kind(&record)
        );
        prior = fields;
    }
    Ok(exit::code(exit::ExitKind::Success))
}
