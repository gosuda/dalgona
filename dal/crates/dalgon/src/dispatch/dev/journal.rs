//! `dalgon dev journal`: replay, diff, torn-tail synthesis, and sidecar reads.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::DevError;
use super::util;
use crate::cli;
use crate::exit;

/// Runs one `dalgon dev journal` subcommand.
pub(super) fn run(args: &cli::DevJournalArgs) -> Result<ExitCode, DevError> {
    match &args.command {
        cli::DevJournalSubcommand::Replay(args) => replay(&args.file),
        cli::DevJournalSubcommand::Diff(args) => diff(&args.before, &args.after),
        cli::DevJournalSubcommand::Torn(args) => torn(&args.input, &args.output),
        cli::DevJournalSubcommand::Sidecar(args) => sidecar(&args.dir, args.name.as_deref()),
    }
}

/// Folds one journal and prints the recovered state summary.
fn replay(path: &Path) -> Result<ExitCode, DevError> {
    let (lines, torn, session) = util::load(path)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(out, "records: {}", lines.len());
    if torn > 0 {
        let _ = writeln!(out, "torn tail: {torn} bytes ignored");
    }
    let _ = writeln!(out, "phase: {:?}", session.phase());
    let _ = writeln!(out, "approval: {:?}", session.approval_mode());
    let model = session
        .active_model()
        .map_or_else(|| "none".to_owned(), |route| format!("{route:?}"));
    let requested = session
        .requested_model()
        .map_or_else(|| "none".to_owned(), |route| format!("{route:?}"));
    let _ = writeln!(out, "active-model: {model}");
    let _ = writeln!(out, "requested-model: {requested}");
    let _ = writeln!(out, "compactions: {}", session.compactions());
    let _ = writeln!(
        out,
        "tokens-since-compaction: {}",
        session.tokens_since_last_compaction()
    );
    let _ = writeln!(out, "compacted-at-leaf: {}", session.compacted_at_leaf());
    let _ = writeln!(out, "wake-run: {}", session.wake_run());
    let _ = writeln!(out, "delivered-jobs: {}", session.delivered_jobs().len());
    let _ = writeln!(
        out,
        "allow-always: {}",
        names(session.allow_always().iter().map(ToString::to_string))
    );
    let _ = writeln!(
        out,
        "promoted: {}",
        names(session.promoted().iter().map(ToString::to_string))
    );
    let _ = writeln!(out, "limits: {:?}", session.limits());
    if let Some(focus) = session.manual_compaction_focus() {
        let _ = writeln!(out, "manual-compaction-focus: {focus}");
    }
    if let Some(reason) = session.should_compact() {
        let _ = writeln!(out, "should-compact: {reason:?}");
    }
    Ok(exit::code(exit::ExitKind::Success))
}

/// Joins a name set into one comma list, or "none".
fn names(iter: impl Iterator<Item = String>) -> String {
    let list: Vec<String> = iter.collect();
    if list.is_empty() {
        "none".to_owned()
    } else {
        list.join(", ")
    }
}

/// Prints the folded fields that differ between two journals.
fn diff(before: &Path, after: &Path) -> Result<ExitCode, DevError> {
    let (_, left_torn, left) = util::load(before)?;
    let (_, right_torn, right) = util::load(after)?;
    let left_fields = util::fields(&left);
    let right_fields = util::fields(&right);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut differences = 0_u32;
    let print =
        |out: &mut dyn Write, mark: char, name: &str, field: &str, differences: &mut u32| {
            *differences += 1;
            let _ = writeln!(out, "{mark} {name}: {field}");
        };
    for (name, value) in &left_fields {
        match right_fields.get(name) {
            None => print(&mut out, '-', name, value, &mut differences),
            Some(kept) if kept != value => {
                print(&mut out, '-', name, value, &mut differences);
                print(&mut out, '+', name, kept, &mut differences);
            }
            Some(_) => {}
        }
    }
    for (name, value) in &right_fields {
        if !left_fields.contains_key(name) {
            print(&mut out, '+', name, value, &mut differences);
        }
    }
    if differences == 0 {
        let _ = writeln!(out, "states identical");
    }
    for (side, torn) in [("before", left_torn), ("after", right_torn)] {
        if torn > 0 {
            let _ = writeln!(out, "{side}: torn tail of {torn} bytes ignored");
        }
    }
    Ok(exit::code(exit::ExitKind::Success))
}

/// Whether two paths name the same file: canonicalized equality catches
/// relative spellings and symlinks, and `same_inode` catches hard links
/// where both paths resolve canonically.
fn same_file(input: &Path, output: &Path) -> bool {
    let source = input.canonicalize().ok();
    if let Ok(target) = output.canonicalize() {
        return Some(target) == source || same_inode(input, output);
    }
    // The output does not exist yet: compare the canonical parent + name
    // against the source so `./journal.jsonl` still names the input.
    let Some(source) = source else {
        return false;
    };
    let Some(dir) = output.parent().and_then(|dir| dir.canonicalize().ok()) else {
        return false;
    };
    output.file_name().map(|name| dir.join(name)).as_deref() == Some(source.as_path())
}

#[cfg(unix)]
fn same_inode(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(left), fs::metadata(right)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    }
}

#[cfg(windows)]
fn same_inode(left: &Path, right: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    // Stable std exposes no by-handle file identity on Windows
    // (rust#63010), so compare every stable metadata field: a hard link
    // shares them all, and refusing a byte-and-time-identical twin is
    // the safe answer either way.
    match (fs::metadata(left), fs::metadata(right)) {
        (Ok(a), Ok(b)) => {
            a.file_attributes() == b.file_attributes()
                && a.file_size() == b.file_size()
                && a.creation_time() == b.creation_time()
                && a.last_access_time() == b.last_access_time()
                && a.last_write_time() == b.last_write_time()
        }
        _ => false,
    }
}

#[cfg(not(any(unix, windows)))]
fn same_inode(_left: &Path, _right: &Path) -> bool {
    false
}

/// Writes `output` as `input` with its final record cut mid-line.
///
/// The last record keeps its leading bytes so the tail still scans as a torn
/// write: a partial record with no terminating newline.
fn torn(input: &Path, output: &Path) -> Result<ExitCode, DevError> {
    if same_file(input, output) {
        return Err(DevError::SamePath {
            path: input.display().to_string(),
        });
    }
    let bytes = fs::read(input).map_err(|source| DevError::Read {
        path: input.display().to_string(),
        source,
    })?;
    let Some(end) = bytes
        .iter()
        .rposition(|byte| *byte != b'\n' && *byte != b'\r')
    else {
        return Err(DevError::Empty {
            path: input.display().to_string(),
        });
    };
    let start = bytes[..end]
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    let last = &bytes[start..=end];
    let mut cut = last.len() * 3 / 5;
    while cut > 0 && std::str::from_utf8(&last[..cut]).is_err() {
        cut -= 1;
    }
    let mut torn = Vec::with_capacity(start + cut);
    torn.extend_from_slice(&bytes[..start]);
    torn.extend_from_slice(&last[..cut]);
    fs::write(output, &torn).map_err(|source| DevError::Write {
        path: output.display().to_string(),
        source,
    })?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let _ = writeln!(
        out,
        "{}: kept {} complete bytes, torn tail of {} bytes",
        output.display(),
        start,
        cut
    );
    Ok(exit::code(exit::ExitKind::Success))
}

/// Lists the sidecar files in a session directory, or dumps one.
fn sidecar(dir: &Path, name: Option<&str>) -> Result<ExitCode, DevError> {
    let journal = dir.join("journal.jsonl");
    if !journal.is_file() {
        return Err(DevError::NotSessionDir {
            path: dir.display().to_string(),
        });
    }
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match name {
        Some(name) => dump(dir, name, &mut out),
        None => list(dir, &journal, &mut out),
    }
}

/// Prints every sidecar file beside the journal, largest last for stability.
fn list(dir: &Path, journal: &Path, out: &mut dyn Write) -> Result<ExitCode, DevError> {
    let mut entries: Vec<(String, u64)> = Vec::new();
    for entry in fs::read_dir(dir).map_err(|source| DevError::Read {
        path: dir.display().to_string(),
        source,
    })? {
        let entry = entry.map_err(|source| DevError::Read {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path == *journal || !path.is_file() {
            continue;
        }
        let size = entry.metadata().map_err(|source| DevError::Read {
            path: path.display().to_string(),
            source,
        })?;
        entries.push((entry.file_name().to_string_lossy().into_owned(), size.len()));
    }
    entries.sort();
    if entries.is_empty() {
        let _ = writeln!(out, "{}: no sidecar files", dir.display());
    }
    for (name, size) in entries {
        let _ = writeln!(out, "{name}\t{size} B");
    }
    Ok(exit::code(exit::ExitKind::Success))
}

/// Dumps one sidecar as UTF-8 text, or a hex listing when it is binary.
fn dump(dir: &Path, name: &str, out: &mut dyn Write) -> Result<ExitCode, DevError> {
    // A sidecar name is one path component: `..`, separators, and
    // absolute spellings must not read outside the session directory.
    let one = Path::new(name).file_name().is_some_and(|file| file == name);
    if !one {
        return Err(DevError::NoSidecar {
            path: dir.display().to_string(),
            name: name.to_owned(),
        });
    }
    let path: PathBuf = dir.join(name);
    // `symlink_metadata` does not follow links: a sidecar that names a
    // symlink resolves outside the session directory and is refused.
    let meta = fs::symlink_metadata(&path).map_err(|_| DevError::NoSidecar {
        path: dir.display().to_string(),
        name: name.to_owned(),
    })?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(DevError::NoSidecar {
            path: dir.display().to_string(),
            name: name.to_owned(),
        });
    }
    let bytes = fs::read(&path).map_err(|source| DevError::Read {
        path: path.display().to_string(),
        source,
    })?;
    match String::from_utf8(bytes) {
        Ok(text) => {
            let _ = write!(out, "{text}");
            if !text.ends_with('\n') {
                let _ = writeln!(out);
            }
        }
        Err(raw) => {
            let bytes = raw.into_bytes();
            for (row, chunk) in bytes.chunks(16).enumerate() {
                let hex: Vec<String> = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
                let _ = writeln!(out, "{:08x}  {}", row * 16, hex.join(" "));
            }
        }
    }
    Ok(exit::code(exit::ExitKind::Success))
}
