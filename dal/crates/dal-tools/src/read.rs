//! The `read` tool: numbered text windows, directory listings, images, and
//! scheme pages.

mod image;
mod window;

use std::{
    fs::{self, File},
    future::Future,
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use dal_agent::{
    ToolError,
    error::SchemeError,
    ext::{
        BoxFuture, Doc, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput, tool::ArgError,
    },
};
use dal_core::{
    ModelInfo, Name, Part, RawJson, RegistrationError, SessionId, ToolClass, ToolData, ToolSpec,
    Workspace,
};
use sonic_rs::{JsonContainerTrait as _, JsonValueTrait as _};

use self::image::{ImageKind, MAX_IMAGE_BYTES};
use self::window::{ReadWindow, Source, render, source_rows, window};
use crate::{
    Seen, digest32,
    evidence::{Binding, capture},
    patch::snapshot::SnapshotStore,
};

const READ_SCHEMA: &str = r#"{"type":"object","properties":{"path":{"type":"string","description":"File, directory, or page to read. Relative paths start at the workspace root. A suffix :<a>-<b>[,<c>[-<d>]] selects lines."},"offset":{"type":"integer","minimum":1,"description":"First line to show, 1-indexed. Default 1."},"limit":{"type":"integer","minimum":1,"maximum":2000,"description":"Maximum number of lines to show. Default 1000."}},"required":["path"],"additionalProperties":false}"#;

const READ_DESCRIPTION: &str = "Read a file, a directory, or a documentation page. A text file shows numbered lines: each line starts with its number and a tab; do not copy the number into patch text. Output stops at limit lines (default 1000, maximum 2000) or 50 KiB, whichever comes first, and the last line gives the offset to continue. Text files larger than 16 MiB are refused; use search to locate content in bigger files. A complete read of a text file ends with [<path>#<tag>]; copy that tag when a patch asks for the file tag. An image (PNG, JPEG, GIF, WebP, at most 5 MiB) returns as an image. A directory lists its entries. Paths may be relative to the workspace root or absolute. Pages: letter://<id> is the source text of an image letter; dal:// is the dal manual; rule://<name> is a rule. Read <scheme>:// alone for its index.";

const DEFAULT_OFFSET: u64 = 1;
const DEFAULT_LIMIT: u64 = 1000;
const MAX_LIMIT: u64 = 2000;
/// Upper bound of the continuation footer: three 20-digit numbers plus text.
const FOOTER_BYTES: usize = 128;
/// Leading bytes probed for image signatures and NUL bytes.
const PROBE_BYTES: usize = 8192;
/// Chunk size of the line-counting pass.
const COUNT_CHUNK: usize = 65536;
const MAX_DIR_ENTRIES: usize = 1000;
/// Largest text file the tool loads: matches the search indexable limit so one
/// read never allocates more than this before the 50 KiB output cap applies.
const MAX_TEXT_BYTES: u64 = 16 << 20;

/// A model-facing `read` failure; the display text is the exact tool text.
#[derive(Debug, thiserror::Error)]
enum ReadError {
    #[error("read: arguments must be an object")]
    NotObject,
    #[error("read: unknown argument \"{0}\"")]
    UnknownArgument(Box<str>),
    #[error("read: {field} must be {kind}")]
    WrongType {
        field: &'static str,
        kind: &'static str,
    },
    #[error("read: path must not be empty")]
    EmptyPath,
    #[error("read: path contains a NUL byte")]
    NulPath,
    #[error("read: offset must be at least 1")]
    Offset,
    #[error("read: limit must be between 1 and 2000")]
    Limit,
    #[error("read: invalid line selector {0}")]
    Selector(Box<str>),
    #[error("read: unknown scheme {0}")]
    UnknownScheme(Box<str>),
    #[error("read: {0} does not exist")]
    Missing(Box<str>),
    #[error("read: {0} is not a file or a directory")]
    NotFileOrDirectory(Box<str>),
    #[error("read: {path} is a binary file ({bytes} bytes)")]
    Binary { path: Box<str>, bytes: u64 },
    #[error("read: {path} is a {bytes}-byte image; the limit is 5 MiB")]
    ImageTooLarge { path: Box<str>, bytes: u64 },
    #[error("read: {path} is a {bytes}-byte text file; the limit is 16 MiB")]
    TextTooLarge { path: Box<str>, bytes: u64 },
    #[error("read: cannot read {path}: {source}")]
    Io { path: Box<str>, source: io::Error },
    #[error("read: the file reader stopped: {0}")]
    Worker(Box<str>),
}

impl From<ReadError> for ToolError {
    fn from(error: ReadError) -> Self {
        ToolError::Failed(Box::new(error))
    }
}

/// Decoded arguments before range validation; `offset` and `limit` keep
/// their JSON sign so out-of-range values get the range error.
struct ReadArgs {
    path: String,
    offset: i128,
    limit: i128,
}

/// The lines a file read shows.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Lines {
    /// `limit` lines starting at `offset`.
    From { offset: u64, limit: u64 },
    /// Sorted, merged, 1-based inclusive intervals from a path selector.
    Select(Vec<(u64, u64)>),
}

/// What one filesystem path holds.
enum FileContent {
    Text(ReadWindow),
    Image { kind: ImageKind, bytes: Vec<u8> },
}

/// Displayed intervals of one file version, for the session's `Seen` store.
#[derive(Debug, Eq, PartialEq)]
struct Shown {
    canonical: String,
    digest: [u8; 32],
    intervals: Vec<(u64, u64)>,
}

/// A finished read: the result parts plus what it displayed.
#[derive(Debug)]
struct Reading {
    parts: Vec<Part>,
    shown: Option<Shown>,
    source: Option<Source>,
}

struct ReadTool {
    name: Name,
    spec: Arc<ToolSpec>,
    seen: Arc<Seen>,
    snapshots: Arc<SnapshotStore>,
}

/// Builds the `read` tool. Displayed text lines are recorded in `seen`,
/// the store the patch tool checks, and each text window is captured in
/// `snapshots` behind its consumer-bound view.
///
/// # Errors
/// Returns [`RegistrationError`] if the fixed name or schema is rejected.
pub(crate) fn tool(
    seen: Arc<Seen>,
    snapshots: Arc<SnapshotStore>,
) -> Result<Arc<dyn Tool>, RegistrationError> {
    let name = Name::parse("read")?;
    let parameters =
        RawJson::parse(READ_SCHEMA).map_err(|_| RegistrationError::InvalidParameters)?;
    let spec = Arc::new(ToolSpec {
        name: name.clone(),
        description: READ_DESCRIPTION.into(),
        parameters,
        grammar: None,
    });
    Ok(Arc::new(ReadTool {
        name,
        spec,
        seen,
        snapshots,
    }))
}

impl Tool for ReadTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _ws: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, call: ToolCall, cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let session = cx.session();
            let root = cx.workspace().as_path().to_path_buf();
            let resolve = |uri: String| {
                let cx = &cx;
                async move { cx.resolve(&uri).await.map(page_text) }
            };
            let result = tokio::select! {
                result = execute(call.args.as_str(), &root, resolve) => result,
                () = cx.cancel().cancelled() => return ToolOutcome::Interrupted,
            };
            match result {
                Ok(reading) => {
                    register(&self.seen, session, &reading);
                    let binding = Binding {
                        session,
                        generation: cx.generation(),
                        consumer: cx.consumer(),
                    };
                    let data =
                        reading.source.map(|source| {
                            let shown = source
                                .rows
                                .iter()
                                .filter(|row| row.complete)
                                .map(|row| row.line);
                            capture(&self.snapshots, binding, &source.path, &source.bytes, shown)
                                .view(&source.path, source.rows, source.truncated)
                        });
                    let data = data.map(ToolData::Read);
                    let mut output = ToolOutput::new(reading.parts);
                    output.data = data;
                    ToolOutcome::Ok(Box::new(output))
                }
                Err(error) => ToolOutcome::Err(error),
            }
        })
    }
}

/// The page text a scheme resolver returned, unchanged.
fn page_text(doc: Doc) -> String {
    doc.text.into()
}

/// Records each displayed interval once.
fn register(seen: &Seen, session: SessionId, reading: &Reading) {
    if let Some(shown) = &reading.shown {
        for &(first, last) in &shown.intervals {
            seen.show(session, &shown.canonical, shown.digest, first, last);
        }
    }
}

/// Runs one read. Scheme URIs go only to `resolve`; every other path is
/// read from disk relative to `root`, without writes.
async fn execute<F, Fut>(raw: &str, root: &Path, resolve: F) -> Result<Reading, ToolError>
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<String, ToolError>>,
{
    let args = decode_args(raw)?;
    if args.path.is_empty() {
        return Err(ReadError::EmptyPath.into());
    }
    if args.path.contains('\0') {
        return Err(ReadError::NulPath.into());
    }
    if let Some(scheme) = uri_scheme(&args.path) {
        let scheme: Box<str> = scheme.into();
        return match resolve(args.path).await {
            Ok(text) => Ok(Reading {
                parts: vec![Part::Text { text: text.into() }],
                shown: None,
                source: None,
            }),
            Err(ToolError::Scheme(SchemeError::Unknown { .. })) => {
                Err(ReadError::UnknownScheme(scheme).into())
            }
            Err(error) => Err(error),
        };
    }
    let offset = u64::try_from(args.offset)
        .ok()
        .filter(|&offset| offset >= 1)
        .ok_or(ReadError::Offset)?;
    let limit = u64::try_from(args.limit)
        .ok()
        .filter(|limit| (1..=MAX_LIMIT).contains(limit))
        .ok_or(ReadError::Limit)?;
    let (path, selection) = parse_selector(&args.path)?;
    let lines = selection.map_or(Lines::From { offset, limit }, Lines::Select);
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || read_path(&root, &path, &lines))
        .await
        .map_err(|error| ReadError::Worker(error.to_string().into()))?
        .map_err(ToolError::from)
}

fn decode_args(raw: &str) -> Result<ReadArgs, ReadError> {
    let value: sonic_rs::Value = sonic_rs::from_str(raw).map_err(|_| ReadError::NotObject)?;
    let object = value.as_object().ok_or(ReadError::NotObject)?;
    let mut path = None;
    let mut offset = i128::from(DEFAULT_OFFSET);
    let mut limit = i128::from(DEFAULT_LIMIT);
    for (key, field) in object {
        match key {
            "path" => {
                let text = field.as_str().ok_or(ReadError::WrongType {
                    field: "path",
                    kind: "a string",
                })?;
                path = Some(text.to_owned());
            }
            "offset" => offset = integer(field, "offset")?,
            "limit" => limit = integer(field, "limit")?,
            other => return Err(ReadError::UnknownArgument(other.into())),
        }
    }
    let path = path.ok_or(ReadError::WrongType {
        field: "path",
        kind: "a string",
    })?;
    Ok(ReadArgs {
        path,
        offset,
        limit,
    })
}

fn integer(field: &sonic_rs::Value, name: &'static str) -> Result<i128, ReadError> {
    field
        .as_i64()
        .map(i128::from)
        .or_else(|| field.as_u64().map(i128::from))
        .ok_or(ReadError::WrongType {
            field: name,
            kind: "an integer",
        })
}

/// The scheme of `path` when it begins with `[a-z][a-z0-9+.-]*://`.
fn uri_scheme(path: &str) -> Option<&str> {
    let (scheme, _) = path.split_once("://")?;
    let mut bytes = scheme.bytes();
    let valid = bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'.' | b'-')
        });
    valid.then_some(scheme)
}

/// Splits a `<path>:<a>-<b>[,<c>[-<d>]]` selector off `path`. A suffix after
/// the last colon is a selector attempt only when it starts with a digit;
/// the returned intervals are sorted and merged.
type Selection = (PathBuf, Option<Vec<(u64, u64)>>);

fn parse_selector(path: &str) -> Result<Selection, ReadError> {
    let Some((base, suffix)) = path.rsplit_once(':') else {
        return Ok((PathBuf::from(path), None));
    };
    if !suffix.starts_with(|c: char| c.is_ascii_digit()) {
        return Ok((PathBuf::from(path), None));
    }
    if base.is_empty() {
        return Err(ReadError::Selector(suffix.into()));
    }
    let invalid = || ReadError::Selector(suffix.into());
    let number = |text: &str| {
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse::<u64>().ok().filter(|&n| n >= 1)
    };
    let mut ranges = Vec::new();
    for item in suffix.split(',') {
        let (first, last) = if let Some((a, b)) = item.split_once('-') {
            (
                number(a).ok_or_else(invalid)?,
                number(b).ok_or_else(invalid)?,
            )
        } else {
            let line = number(item).ok_or_else(invalid)?;
            (line, line)
        };
        if first > last {
            return Err(invalid());
        }
        ranges.push((first, last));
    }
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (first, last) in ranges {
        match merged.last_mut() {
            Some(previous) if first <= previous.1.saturating_add(1) => {
                previous.1 = previous.1.max(last);
            }
            _ => merged.push((first, last)),
        }
    }
    Ok((PathBuf::from(base), Some(merged)))
}

/// Reads one filesystem path and renders the result.
fn read_path(root: &Path, path: &Path, lines: &Lines) -> Result<Reading, ReadError> {
    let target = root.join(path);
    let shown = path.to_string_lossy();
    let metadata = fs::metadata(&target).map_err(|error| io_error(&shown, error))?;
    if metadata.is_dir() {
        let text = read_directory(&target, &shown)?;
        return Ok(Reading {
            parts: vec![Part::Text { text: text.into() }],
            shown: None,
            source: None,
        });
    }
    if !metadata.is_file() {
        return Err(ReadError::NotFileOrDirectory(shown.into()));
    }
    let tag_path = display_path(root, &target, &shown);
    match read_file(&target, &shown, lines, footer_reserve(&tag_path))? {
        FileContent::Image { kind, bytes } => Ok(Reading {
            parts: vec![
                Part::Text {
                    text: format!("Read image file [{}]", kind.mime()).into(),
                },
                Part::Image {
                    mime: kind.mime().into(),
                    bytes: bytes.into(),
                },
            ],
            shown: None,
            source: None,
        }),
        FileContent::Text(window) => {
            let text = render(&window, &tag_path);
            let ReadWindow {
                lines,
                next_offset,
                intervals,
                bytes,
                ..
            } = window;
            let shown = if intervals.is_empty() {
                None
            } else {
                let canonical =
                    fs::canonicalize(&target).map_err(|error| io_error(&shown, error))?;
                Some(Shown {
                    canonical: canonical.to_string_lossy().into_owned(),
                    digest: digest32(&bytes),
                    intervals,
                })
            };
            Ok(Reading {
                parts: vec![Part::Text { text: text.into() }],
                shown,
                source: Some(Source {
                    path: tag_path,
                    bytes,
                    rows: source_rows(lines),
                    truncated: next_offset.is_some(),
                }),
            })
        }
    }
}

fn io_error(shown: &str, error: io::Error) -> ReadError {
    if error.kind() == io::ErrorKind::NotFound {
        ReadError::Missing(shown.into())
    } else {
        ReadError::Io {
            path: shown.into(),
            source: error,
        }
    }
}

/// The tag-line path: workspace-relative with `/` separators when the file
/// is under `root`, otherwise the path as given.
fn display_path(root: &Path, target: &Path, shown: &str) -> String {
    match target.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_string_lossy()),
                Component::ParentDir => Some("..".into()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/"),
        _ => shown.to_owned(),
    }
}

/// Lists a directory: directories first with a trailing `/`, each group
/// sorted bytewise, at most [`MAX_DIR_ENTRIES`] entries.
fn read_directory(path: &Path, shown: &str) -> Result<String, ReadError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).map_err(|error| io_error(shown, error))? {
        let entry = entry.map_err(|error| io_error(shown, error))?;
        let is_dir = fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_dir());
        entries.push((!is_dir, entry.file_name()));
    }
    entries
        .sort_unstable_by(|a, b| (a.0, a.1.as_encoded_bytes()).cmp(&(b.0, b.1.as_encoded_bytes())));
    let mut lines: Vec<String> = entries
        .iter()
        .take(MAX_DIR_ENTRIES)
        .map(|(is_file, name)| {
            let mut line = name.to_string_lossy().into_owned();
            if !is_file {
                line.push('/');
            }
            line
        })
        .collect();
    if entries.len() > MAX_DIR_ENTRIES {
        lines.push(format!(
            "[Truncated: {} more entries]",
            entries.len() - MAX_DIR_ENTRIES
        ));
    }
    Ok(lines.join("\n"))
}

/// Bytes kept free for the footer or the tag line of `tag_path`.
fn footer_reserve(tag_path: &str) -> usize {
    (tag_path.len() + "[#]".len() + 8).max(FOOTER_BYTES)
}

/// Reads a regular file: an image by signature, a binary refusal, or a
/// numbered text window. Line totals come from a first counting pass; the
/// window decodes the whole second-pass byte sequence lossily at once.
fn read_file(
    path: &Path,
    shown: &str,
    lines: &Lines,
    reserve: usize,
) -> Result<FileContent, ReadError> {
    let fail = |error| io_error(shown, error);
    let mut file = File::open(path).map_err(fail)?;
    let size = file.metadata().map_err(fail)?.len();
    let mut prefix = [0_u8; PROBE_BYTES];
    let probed = fill(&mut file, &mut prefix).map_err(fail)?;
    let prefix = &prefix[..probed];
    if let Some(kind) = ImageKind::detect(prefix) {
        if size > MAX_IMAGE_BYTES {
            return Err(ReadError::ImageTooLarge {
                path: shown.into(),
                bytes: size,
            });
        }
        let Some(bytes) = reread(&mut file, MAX_IMAGE_BYTES).map_err(fail)? else {
            return Err(ReadError::ImageTooLarge {
                path: shown.into(),
                bytes: size,
            });
        };
        return Ok(FileContent::Image { kind, bytes });
    }
    if prefix.contains(&0) {
        return Err(ReadError::Binary {
            path: shown.into(),
            bytes: size,
        });
    }
    if size > MAX_TEXT_BYTES {
        return Err(ReadError::TextTooLarge {
            path: shown.into(),
            bytes: size,
        });
    }
    file.seek(SeekFrom::Start(0)).map_err(fail)?;
    let total = count_lines(&mut file).map_err(fail)?;
    let Some(bytes) = reread(&mut file, MAX_TEXT_BYTES).map_err(fail)? else {
        return Err(ReadError::TextTooLarge {
            path: shown.into(),
            bytes: size,
        });
    };
    Ok(FileContent::Text(window(bytes, total, lines, reserve)))
}

fn fill(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn reread(file: &mut File, limit: u64) -> io::Result<Option<Vec<u8>>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Ok(None);
    }
    Ok(Some(bytes))
}

/// Counts lines as newline bytes plus one for a final unterminated line.
fn count_lines(file: &mut File) -> io::Result<u64> {
    let mut buffer = vec![0_u8; COUNT_CHUNK];
    let mut total = 0_u64;
    let mut last = None;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let chunk = &buffer[..read];
        total += chunk.split(|byte| *byte == b'\n').count().saturating_sub(1) as u64;
        last = chunk.last().copied();
    }
    if last.is_some_and(|byte| byte != b'\n') {
        total += 1;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs, future::Ready, path::Path};

    use dal_agent::{ToolError, error::SchemeError};
    use dal_core::{Part, SessionId};

    use super::window::MAX_OUTPUT_BYTES;
    use super::{
        FileContent, Lines, MAX_TEXT_BYTES, ReadWindow, Reading, execute, footer_reserve,
        parse_selector, read_file, register, render,
    };
    use crate::{Seen, tag8};

    #[expect(
        clippy::needless_pass_by_value,
        reason = "the page-callback seam takes an owned uri"
    )]
    fn no_pages(uri: String) -> Ready<Result<String, ToolError>> {
        std::future::ready(Err(ToolError::Scheme(SchemeError::Failed {
            message: format!("unexpected page request {uri}").into(),
        })))
    }

    async fn read(root: &Path, args: &str) -> Result<Reading, ToolError> {
        execute(args, root, no_pages).await
    }

    fn text(reading: &Reading) -> &str {
        let first = match reading.parts.first() {
            Some(Part::Text { text }) => Some(&**text),
            _ => None,
        };
        first.unwrap()
    }

    fn text_window(path: &Path, shown: &str, offset: u64, limit: u64) -> ReadWindow {
        let lines = Lines::From { offset, limit };
        let window = match read_file(path, shown, &lines, footer_reserve(shown)).unwrap() {
            FileContent::Text(window) => Some(window),
            FileContent::Image { .. } => None,
        };
        window.unwrap()
    }

    fn args(path: &str) -> String {
        sonic_rs::to_string(&sonic_rs::json!({ "path": path })).unwrap()
    }

    fn whole_tag(path: &str, bytes: &[u8]) -> String {
        format!("[{path}#{}]", tag8("whole", bytes))
    }

    #[tokio::test]
    async fn read_window_tiling() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=3500).fold(String::new(), |mut body, n| {
            let _ = std::fmt::Write::write_fmt(&mut body, format_args!("line {n}\n"));
            body
        });
        fs::write(dir.path().join("big.txt"), &body).unwrap();
        let mut offset = 1_u64;
        let mut numbers = Vec::new();
        let mut reads = 0;
        loop {
            reads += 1;
            let reading = read(
                dir.path(),
                &format!(r#"{{"path":"big.txt","offset":{offset}}}"#),
            )
            .await
            .unwrap();
            let rendered = text(&reading).to_owned();
            let (rows, footer) = rendered.rsplit_once('\n').unwrap();
            for row in rows.lines() {
                let (number, rest) = row.split_once('\t').unwrap();
                let number: u64 = number.parse().unwrap();
                assert_eq!(rest, format!("line {number}"));
                numbers.push(number);
            }
            let shown = reading.shown.unwrap();
            assert_eq!(shown.intervals.len(), 1);
            if let Some(next) = footer
                .strip_prefix("[Showing lines ")
                .and_then(|rest| rest.split_once(". Use :"))
                .map(|(_, rest)| {
                    rest.trim_end_matches(" to continue.]")
                        .parse::<u64>()
                        .unwrap()
                })
            {
                assert_eq!(
                    footer,
                    format!(
                        "[Showing lines {offset}-{} of 3500. Use :{next} to continue.]",
                        next - 1
                    )
                );
                offset = next;
            } else {
                assert_eq!(footer, whole_tag("big.txt", body.as_bytes()));
                break;
            }
        }
        assert_eq!(reads, 4);
        assert_eq!(numbers, (1..=3500).collect::<Vec<u64>>());
    }

    #[tokio::test]
    async fn read_total_line_honesty() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"alpha\r\nbeta\r\ngamma";
        fs::write(dir.path().join("crlf.txt"), bytes).unwrap();
        let newlines = bytes.split(|b| *b == b'\n').count().saturating_sub(1) as u64;
        let expected_total = newlines + 1;
        let first = read(dir.path(), r#"{"path":"crlf.txt","limit":1}"#)
            .await
            .unwrap();
        assert_eq!(
            text(&first),
            format!("1\talpha\r\n[Showing lines 1-1 of {expected_total}. Use :2 to continue.]")
        );
        let rest = read(dir.path(), r#"{"path":"crlf.txt","offset":2}"#)
            .await
            .unwrap();
        assert_eq!(
            text(&rest),
            format!("2\tbeta\r\n3\tgamma\n{}", whole_tag("crlf.txt", bytes))
        );
        let past = read(dir.path(), r#"{"path":"crlf.txt","offset":9}"#)
            .await
            .unwrap();
        assert_eq!(text(&past), "[No lines at or after 9; file has 3 lines.]");
        assert!(past.shown.is_none());
        fs::write(dir.path().join("empty.txt"), b"").unwrap();
        let empty = read(dir.path(), &args("empty.txt")).await.unwrap();
        assert_eq!(text(&empty), whole_tag("empty.txt", b""));
    }

    #[test]
    fn read_lossy_utf8_decode() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = Vec::new();
        while bytes.len() < 65535 - 99 {
            bytes.extend_from_slice(&[b'x'; 99]);
            bytes.push(b'\n');
        }
        bytes.resize(65535, b'y');
        // A three-byte sequence starts at 65535 with its second byte invalid.
        bytes.extend_from_slice(&[0xE2, 0xFF, 0xAC, b'\n']);
        while bytes.len() < 131_070 {
            bytes.extend_from_slice(&[b'z'; 49]);
            bytes.push(b'\n');
        }
        bytes.resize(131_070, b'w');
        // A valid euro sign straddles the second 65536-byte boundary.
        bytes.extend_from_slice("€\n".as_bytes());
        let path = dir.path().join("lossy.txt");
        fs::write(&path, &bytes).unwrap();

        let expected = String::from_utf8_lossy(&bytes).into_owned();
        let expected_lines: Vec<&str> = expected.strip_suffix('\n').unwrap().split('\n').collect();
        let mut lines = Vec::new();
        let mut offset = 1;
        loop {
            let window = text_window(&path, "lossy.txt", offset, 2000);
            assert_eq!(window.total_lines, expected_lines.len() as u64);
            assert!(render(&window, "lossy.txt").len() <= MAX_OUTPUT_BYTES);
            let next_offset = window.next_offset;
            lines.extend(window.lines.into_iter().map(|line| line.text));
            match next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(lines, expected_lines);
        let replacements = |text: &str| text.matches('\u{FFFD}').count();
        let shown_replacements: usize = lines.iter().map(String::as_str).map(replacements).sum();
        assert_eq!(shown_replacements, replacements(&expected));
        assert!(lines.iter().any(|line| line.ends_with('€')));
    }

    #[test]
    fn read_long_line_cut() {
        let dir = tempfile::tempdir().unwrap();
        let line = format!("a{}", "é".repeat(2500));
        assert_eq!(line.len(), 5001);
        let path = dir.path().join("long.txt");
        fs::write(&path, &line).unwrap();
        let window = text_window(&path, "long.txt", 1, 1);
        let shown = &window.lines[0];
        assert!(shown.was_truncated);
        assert_eq!(shown.text, format!("a{}...", "é".repeat(999)));
        assert_eq!(shown.text.len(), 1999 + 3);
        assert_eq!(window.next_offset, None);
        assert_eq!(window.intervals, [] as [(u64, u64); 0]);
    }

    #[tokio::test]
    async fn read_image_signatures() {
        let dir = tempfile::tempdir().unwrap();
        let png = [
            &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A][..],
            &[0; 16],
        ]
        .concat();
        let cases: [(&str, Vec<u8>, &str); 4] = [
            ("a.png", png.clone(), "image/png"),
            (
                "a.jpg",
                [&[0xFF, 0xD8, 0xFF, 0xE0][..], &[0; 16]].concat(),
                "image/jpeg",
            ),
            ("a.gif", [&b"GIF89a"[..], &[0; 16]].concat(), "image/gif"),
            (
                "a.webp",
                [&b"RIFF\0\0\0\0WEBPVP8 "[..], &[0; 16]].concat(),
                "image/webp",
            ),
        ];
        for (name, bytes, mime) in &cases {
            fs::write(dir.path().join(name), bytes).unwrap();
            let reading = read(dir.path(), &args(name)).await.unwrap();
            assert_eq!(
                reading.parts,
                vec![
                    Part::Text {
                        text: format!("Read image file [{mime}]").into()
                    },
                    Part::Image {
                        mime: (*mime).into(),
                        bytes: bytes.clone().into()
                    },
                ]
            );
            assert!(reading.shown.is_none());
        }
        let mut large = png;
        large.resize(6 * 1024 * 1024, 0);
        fs::write(dir.path().join("large.png"), &large).unwrap();
        let error = read(dir.path(), &args("large.png")).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "read: large.png is a 6291456-byte image; the limit is 5 MiB"
        );
    }
    #[tokio::test]
    async fn read_text_file_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        fs::write(&path, vec![b'a'; 8192]).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_TEXT_BYTES + 1)
            .unwrap();
        let error = read(dir.path(), &args("big.txt")).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "read: big.txt is a {}-byte text file; the limit is 16 MiB",
                MAX_TEXT_BYTES + 1
            )
        );
    }

    #[tokio::test]
    async fn read_binary_file_detection() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("data.bin"), b"abc\0def").unwrap();
        let error = read(dir.path(), &args("data.bin")).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "read: data.bin is a binary file (7 bytes)"
        );

        let mut late = vec![b'a'; 8192];
        late.push(0);
        fs::write(dir.path().join("late.txt"), &late).unwrap();
        let reading = read(dir.path(), &args("late.txt")).await.unwrap();
        assert!(text(&reading).starts_with("1\t"));
    }

    #[tokio::test]
    async fn read_directory_listing() {
        let dir = tempfile::tempdir().unwrap();
        let listed = dir.path().join("tree");
        fs::create_dir(&listed).unwrap();
        for index in 0..600 {
            fs::create_dir(listed.join(format!("z{index:04}"))).unwrap();
            fs::write(listed.join(format!("a{index:04}")), b"").unwrap();
        }
        let reading = read(dir.path(), &args("tree")).await.unwrap();
        let mut expected: Vec<String> = (0..600).map(|index| format!("z{index:04}/")).collect();
        expected.extend((0..400).map(|index| format!("a{index:04}")));
        expected.push("[Truncated: 200 more entries]".to_owned());
        assert_eq!(text(&reading), expected.join("\n"));
        assert!(reading.shown.is_none());

        #[cfg(unix)]
        {
            let small = dir.path().join("small");
            fs::create_dir_all(small.join("Sub")).unwrap();
            fs::write(small.join("B"), b"").unwrap();
            fs::write(small.join("a"), b"").unwrap();
            std::os::unix::fs::symlink(&small, dir.path().join("link")).unwrap();
            let linked = read(dir.path(), &args("link")).await.unwrap();
            assert_eq!(text(&linked), "Sub/\nB\na");
        }
    }

    #[tokio::test]
    async fn read_scheme_resolution() {
        let dir = tempfile::tempdir().unwrap();
        // Disk decoys: a scheme read that fell through to the filesystem would return these.
        // Windows forbids `:` in file names, so there is nothing to plant; a
        // fallthrough would fail with InvalidFilename instead of reading DISK.
        #[cfg(unix)]
        for decoy in ["letter:/7", "letter:/nope", "dalgon:/config"] {
            let path = dir.path().join(decoy);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "DISK").unwrap();
        }
        let requirements = "# Requirements\n\nThe config page.\n";
        let calls = Cell::new(0);
        let pages = |uri: String| {
            calls.set(calls.get() + 1);
            std::future::ready(match uri.as_str() {
                "letter://7" => Ok("letter source text".to_owned()),
                "dal://config" => Ok(requirements.to_owned()),
                "letter://nope" => Err(ToolError::Scheme(SchemeError::Failed {
                    message: "read: no letter nope in this session".into(),
                })),
                other => Err(ToolError::Scheme(SchemeError::Unknown {
                    scheme: other.split_once("://").unwrap().0.into(),
                })),
            })
        };
        let letter = execute(
            r#"{"path":"letter://7","offset":0,"limit":0}"#,
            dir.path(),
            pages,
        )
        .await
        .unwrap();
        assert_eq!(text(&letter), "letter source text");
        assert!(letter.shown.is_none());
        let config = execute(&args("dal://config"), dir.path(), pages)
            .await
            .unwrap();
        assert_eq!(text(&config), requirements);
        let missing = execute(&args("letter://nope"), dir.path(), pages)
            .await
            .unwrap_err();
        assert_eq!(missing.to_string(), "read: no letter nope in this session");
        let unknown = execute(&args("foo://bar"), dir.path(), pages)
            .await
            .unwrap_err();
        assert_eq!(unknown.to_string(), "read: unknown scheme foo");
        assert_eq!(calls.get(), 4);
        let bad_type = execute(r#"{"path":"letter://7","offset":"1"}"#, dir.path(), pages)
            .await
            .unwrap_err();
        assert_eq!(bad_type.to_string(), "read: offset must be an integer");
        assert_eq!(calls.get(), 4);
    }

    #[tokio::test]
    async fn read_error_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let cases = [
            (r#"{"path":""}"#, "read: path must not be empty"),
            (r#"{"path":"","offset":0}"#, "read: path must not be empty"),
            (r#"{"path":"a\u0000b"}"#, "read: path contains a NUL byte"),
            (
                r#"{"path":"missing.txt"}"#,
                "read: missing.txt does not exist",
            ),
            (
                r#"{"path":"a.txt","offset":0}"#,
                "read: offset must be at least 1",
            ),
            (
                r#"{"path":"a.txt","offset":-3}"#,
                "read: offset must be at least 1",
            ),
            (
                r#"{"path":"a.txt","limit":0}"#,
                "read: limit must be between 1 and 2000",
            ),
            (
                r#"{"path":"a.txt","limit":2001}"#,
                "read: limit must be between 1 and 2000",
            ),
            (
                r#"{"path":"a.txt","bogus":1}"#,
                "read: unknown argument \"bogus\"",
            ),
            (r#"{"path":7}"#, "read: path must be a string"),
            (
                r#"{"path":"a.txt","limit":1.5}"#,
                "read: limit must be an integer",
            ),
            (r#"{"path":"a.txt:4-2"}"#, "read: invalid line selector 4-2"),
            (r#"{"path":"a.txt:1-x"}"#, "read: invalid line selector 1-x"),
            (r#"{"path":"a.txt:0"}"#, "read: invalid line selector 0"),
            (
                r#"{"path":"a.txt:1,,2"}"#,
                "read: invalid line selector 1,,2",
            ),
        ];
        for (input, expected) in cases {
            let error = read(dir.path(), input).await.unwrap_err();
            assert_eq!(error.to_string(), expected, "input {input}");
        }
        assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"one\ntwo\n");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn read_line_selector() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=12).fold(String::new(), |mut body, n| {
            let _ = std::fmt::Write::write_fmt(&mut body, format_args!("l{n}\n"));
            body
        });
        fs::write(dir.path().join("a.rs"), &body).unwrap();

        let partial = read(dir.path(), &args("a.rs:4-6,10")).await.unwrap();
        assert_eq!(
            text(&partial),
            "4\tl4\n5\tl5\n6\tl6\n10\tl10\n[Showing lines 4-10 of 12. Use :11 to continue.]"
        );
        let shown = partial.shown.as_ref().unwrap();
        assert_eq!(shown.intervals, vec![(4, 6), (10, 10)]);
        let seen = Seen::new();
        let session = SessionId::new_v7();
        register(&seen, session, &partial);
        assert_eq!(
            seen.intervals(session, &shown.canonical, shown.digest),
            vec![(4, 6), (10, 10)]
        );

        let whole = read(dir.path(), &args("a.rs:7-12,1-6")).await.unwrap();
        let expected: String = (1..=12).fold(String::new(), |mut expected, n| {
            let _ = std::fmt::Write::write_fmt(&mut expected, format_args!("{n}\tl{n}\n"));
            expected
        });
        assert_eq!(
            text(&whole),
            format!("{expected}{}", whole_tag("a.rs", body.as_bytes()))
        );
        let whole_shown = whole.shown.as_ref().unwrap();
        assert_eq!(whole_shown.intervals, vec![(1, 12)]);
        register(&seen, session, &whole);
        assert_eq!(
            seen.intervals(session, &whole_shown.canonical, whole_shown.digest),
            vec![(1, 12)]
        );

        let offset_ignored = read(dir.path(), r#"{"path":"a.rs:2","offset":9,"limit":13}"#)
            .await
            .unwrap();
        assert_eq!(
            text(&offset_ignored),
            "2\tl2\n[Showing lines 2-2 of 12. Use :3 to continue.]"
        );

        assert_eq!(
            parse_selector(r"C:\file").unwrap(),
            (r"C:\file".into(), None)
        );
        assert_eq!(
            parse_selector("notes:draft").unwrap(),
            ("notes:draft".into(), None)
        );
        assert_eq!(
            parse_selector("a.rs:10:3").unwrap(),
            ("a.rs:10".into(), Some(vec![(3, 3)]))
        );
    }
}
