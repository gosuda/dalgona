//! The on-disk sections of the workspace index: their format, their codecs,
//! publication, and the validated snapshots they open into.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::INDEX_FORMAT_VERSION;
use super::IndexError;
use super::build::{PostingsWriter, Run, Scratch, merge};
use super::find::{self, FindGlob, FindResult, RawEntry};
use super::query::intersect;

pub(super) const FILES: &str = "files.bin";
pub(super) const EXTRA: &str = "files-extra.bin";
pub(super) const POSTINGS: &str = "postings.bin";
pub(super) const STAMPS: &str = "stamps.bin";
pub(super) const META: &str = "meta.bin";

const MAGIC_FILES: [u8; 4] = *b"DALF";
const MAGIC_EXTRA: [u8; 4] = *b"DALX";
pub(super) const MAGIC_POSTINGS: [u8; 4] = *b"DALP";
const MAGIC_STAMPS: [u8; 4] = *b"DALS";

pub(super) const HEADER_LEN: usize = 16;
pub(super) const POSTING_LEN: usize = 6;
const TABLE_ENTRY_LEN: usize = 16;
pub(super) const FOOTER_LEN: usize = 16;
const STAMP_LEN: usize = 20;
pub(super) const GRAM_LIMIT: u32 = 1 << 24;

pub(super) const HEX: &[u8; 16] = b"0123456789abcdef";

static SEQ: AtomicU64 = AtomicU64::new(0);

pub(super) fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}

fn new_nonce() -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&std::process::id().to_le_bytes());
    hasher.update(&next_seq().to_le_bytes());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    hasher.update(&now.as_nanos().to_le_bytes());
    let mut first = [0; 8];
    first.copy_from_slice(&hasher.finalize().as_bytes()[..8]);
    u64::from_le_bytes(first)
}

/// Pack one 3-byte window injectively.
pub(super) fn pack(a: u8, b: u8, c: u8) -> u32 {
    (u32::from(a) << 16) | (u32::from(b) << 8) | u32::from(c)
}

pub(super) fn next_bit(byte: u8) -> u8 {
    1 << (byte & 7)
}

/// Modification time and size; the stat-walk identity of a file's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileStamp {
    secs: i64,
    nanos: u32,
    size: u64,
}

impl FileStamp {
    pub(super) fn of(meta: &fs::Metadata) -> Option<Self> {
        let modified = meta.modified().ok()?;
        let (secs, nanos) = match modified.duration_since(UNIX_EPOCH) {
            Ok(after) => (i64::try_from(after.as_secs()).ok()?, after.subsec_nanos()),
            Err(before) => {
                let before = before.duration();
                (
                    -i64::try_from(before.as_secs()).ok()? - 1,
                    before.subsec_nanos(),
                )
            }
        };
        Some(Self {
            secs,
            nanos,
            size: meta.len(),
        })
    }
}

/// Why a listable file carries no postings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExtraKind {
    Dir,
    Binary,
    Oversize,
    Utf16,
    Unreadable,
}

impl ExtraKind {
    fn code(self) -> u8 {
        match self {
            Self::Dir => 0,
            Self::Binary => 1,
            Self::Oversize => 2,
            Self::Utf16 => 3,
            Self::Unreadable => 4,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Dir,
            1 => Self::Binary,
            2 => Self::Oversize,
            3 => Self::Utf16,
            4 => Self::Unreadable,
            _ => return None,
        })
    }
}

/// The validated content of one published index.
pub(super) struct Data {
    pub(super) files: Vec<PathBuf>,
    pub(super) extra: Vec<(PathBuf, ExtraKind)>,
    /// `files` stamps, then `extra` stamps; `None` for directories and unreadable files.
    pub(super) stamps: Vec<Option<FileStamp>>,
    pub(super) postings: Postings,
}

impl Data {
    /// The no-index-root state: an empty universe no query ever reads.
    pub(super) fn empty() -> Self {
        Self {
            files: Vec::new(),
            extra: Vec::new(),
            stamps: Vec::new(),
            postings: Postings::empty(),
        }
    }
}

pub(super) enum Old {
    File(u32),
    Extra(ExtraKind),
}

/// The previous entry to keep for `raw`, when its stamp proves it unchanged.
pub(super) fn keep<'a>(
    old: &'a HashMap<&Path, (Old, Option<FileStamp>)>,
    raw: &RawEntry,
) -> Option<&'a Old> {
    let (entry, stamp) = old.get(raw.path.as_path())?;
    let kept = match entry {
        Old::Extra(ExtraKind::Dir) => raw.is_dir,
        _ => !raw.is_dir && stamp.is_some() && *stamp == FileStamp::of(&raw.meta),
    };
    kept.then_some(entry)
}

impl Data {
    pub(super) fn old_entries(&self) -> HashMap<&Path, (Old, Option<FileStamp>)> {
        let mut old = HashMap::with_capacity(self.files.len() + self.extra.len());
        let mut stamps = self.stamps.iter().copied();
        for (id, path) in (0_u32..).zip(&self.files) {
            old.insert(path.as_path(), (Old::File(id), stamps.next().flatten()));
        }
        for (path, kind) in &self.extra {
            old.insert(path.as_path(), (Old::Extra(*kind), stamps.next().flatten()));
        }
        old
    }

    /// True when the stat walk shows exactly the stamped universe of this data.
    pub(super) fn matches(&self, listing: &[RawEntry]) -> bool {
        if listing.len() != self.files.len() + self.extra.len() {
            return false;
        }
        let old = self.old_entries();
        listing.iter().all(|raw| keep(&old, raw).is_some())
    }

    pub(super) fn candidates(
        &self,
        clauses: &[Vec<Vec<u8>>],
        ignore_case: bool,
        scope: Option<&Path>,
    ) -> Option<Vec<PathBuf>> {
        let mut narrowed: Option<Vec<u32>> = None;
        for clause in clauses {
            if clause.is_empty() || clause.iter().any(|literal| literal.len() < 3) {
                continue;
            }
            let mut union = Vec::new();
            for literal in clause {
                let literal = if ignore_case {
                    literal.to_ascii_lowercase()
                } else {
                    literal.clone()
                };
                union.extend(self.postings.literal_files(&literal, self.files.len())?);
            }
            union.sort_unstable();
            union.dedup();
            narrowed = Some(match narrowed {
                None => union,
                Some(prev) => intersect(&prev, &union),
            });
        }
        let in_scope = |path: &&PathBuf| {
            scope.is_none_or(|scope| scope.as_os_str().is_empty() || path.starts_with(scope))
        };
        let ids = narrowed.unwrap_or_else(|| (0_u32..).take(self.files.len()).collect());
        Some(
            ids.into_iter()
                .filter_map(|id| self.files.get(id as usize))
                .filter(in_scope)
                .cloned()
                .collect(),
        )
    }

    pub(super) fn find(
        &self,
        scope: Option<&Path>,
        glob: &FindGlob,
        limit: usize,
    ) -> Option<FindResult> {
        let scope = scope.filter(|scope| !scope.as_os_str().is_empty());
        if let Some(scope) = scope {
            let known_dir = self
                .extra
                .binary_search_by(|(path, _)| find::path_bytes_cmp(path, scope))
                .is_ok_and(|at| self.extra[at].1 == ExtraKind::Dir);
            if !known_dir {
                return None;
            }
        }
        let items = self
            .files
            .iter()
            .map(|path| (path.as_path(), false))
            .chain(
                self.extra
                    .iter()
                    .map(|(path, kind)| (path.as_path(), *kind == ExtraKind::Dir)),
            )
            .filter_map(|(path, is_dir)| match scope {
                None => Some((path, is_dir)),
                Some(scope) => path
                    .strip_prefix(scope)
                    .ok()
                    .filter(|rest| !rest.as_os_str().is_empty())
                    .map(|rest| (rest, is_dir)),
            });
        Some(find::select(items, glob, limit))
    }
}

/// The validated `postings.bin` buffer.
pub(super) struct Postings {
    pub(super) bytes: Arc<[u8]>,
    pub(super) grams: usize,
    pub(super) table_at: usize,
}

impl Postings {
    fn empty() -> Self {
        let mut bytes = header(MAGIC_POSTINGS, 0);
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        bytes.extend_from_slice(&0_u64.to_le_bytes());
        Self {
            bytes: bytes.into(),
            grams: 0,
            table_at: HEADER_LEN,
        }
    }

    /// Validate the shape and the gram table of a published postings file.
    /// Posting file ids are checked when a gram list is used, so a warm open
    /// never scans the whole file.
    fn parse(bytes: Arc<[u8]>, nonce: u64) -> Option<Self> {
        check_header(&bytes, MAGIC_POSTINGS, nonce)?;
        let footer = bytes.len().checked_sub(FOOTER_LEN)?;
        let count = usize::try_from(u64_at(&bytes, footer)?).ok()?;
        let grams = usize::try_from(u64_at(&bytes, footer + 8)?).ok()?;
        let table_at = count.checked_mul(POSTING_LEN)?.checked_add(HEADER_LEN)?;
        let end = grams.checked_mul(TABLE_ENTRY_LEN)?.checked_add(table_at)?;
        if end != footer {
            return None;
        }
        let postings = Self {
            bytes,
            grams,
            table_at,
        };
        let mut running = 0_usize;
        let mut previous = None;
        for at in 0..grams {
            let (gram, len, start) = postings.entry(at)?;
            if gram >= GRAM_LIMIT || previous.is_some_and(|p| p >= gram) || start != running {
                return None;
            }
            previous = Some(gram);
            running = running.checked_add(len)?;
        }
        (running == count).then_some(postings)
    }
}

/// The checked state of one gram's posting list.
pub(super) enum GramList {
    /// The gram is unused: no file holds it.
    Absent,
    /// A valid posting list: `(start, len)` into the postings section.
    List(usize, usize),
}

impl Postings {
    /// The ids of one used posting list, checked to ascend strictly and name a
    /// file of the path table; `None` marks the section corrupt.
    pub(super) fn checked_list(&self, gram: u32, files: usize) -> Option<GramList> {
        let Some((start, len)) = self.list(gram) else {
            return Some(GramList::Absent);
        };
        let mut previous = None;
        for at in start..start + len {
            let (file, _, _) = self.record(at)?;
            if (file as usize) >= files || previous.is_some_and(|p| p >= file) {
                return None;
            }
            previous = Some(file);
        }
        Some(GramList::List(start, len))
    }

    pub(super) fn entry(&self, at: usize) -> Option<(u32, usize, usize)> {
        let base = self.table_at + at * TABLE_ENTRY_LEN;
        let gram = u32_at(&self.bytes, base)?;
        let len = usize::try_from(u32_at(&self.bytes, base + 4)?).ok()?;
        let start = usize::try_from(u64_at(&self.bytes, base + 8)?).ok()?;
        Some((gram, len, start))
    }

    pub(super) fn record(&self, at: usize) -> Option<(u32, u8, u8)> {
        let base = HEADER_LEN + at * POSTING_LEN;
        let record = self.bytes.get(base..base + POSTING_LEN)?;
        Some((u32_at(record, 0)?, record[4], record[5]))
    }

    /// The posting range `(start, len)` of one gram.
    pub(super) fn list(&self, gram: u32) -> Option<(usize, usize)> {
        let (mut low, mut high) = (0, self.grams);
        while low < high {
            let mid = low + (high - low) / 2;
            let (found, len, start) = self.entry(mid)?;
            match found.cmp(&gram) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some((start, len)),
            }
        }
        None
    }

    /// The masks of `file` inside one posting range.
    pub(super) fn masks(&self, (start, len): (usize, usize), file: u32) -> Option<(u8, u8)> {
        let (mut low, mut high) = (start, start + len);
        while low < high {
            let mid = low + (high - low) / 2;
            let (found, loc, next) = self.record(mid)?;
            match found.cmp(&file) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some((loc, next)),
            }
        }
        None
    }
}

pub(super) struct Sections {
    pub(super) files: Vec<PathBuf>,
    pub(super) extra: Vec<(PathBuf, ExtraKind)>,
    pub(super) stamps: Vec<Option<FileStamp>>,
}

/// Write every section to a temporary name, rename them into place with
/// `meta.bin` last, and load the published postings back.
pub(super) fn publish(
    dir: &Path,
    canonical: &Path,
    sections: Sections,
    runs: Vec<Run<'_>>,
    scratch: &mut Scratch,
) -> Result<Data, IndexError> {
    let nonce = new_nonce();
    let temp = |name: &str| dir.join(format!("{name}.tmp-{}-{}", std::process::id(), next_seq()));
    let postings_temp = temp(POSTINGS);
    scratch.0.push(postings_temp.clone());
    let mut writer = PostingsWriter::create(&postings_temp, dir, nonce, scratch)?;
    merge(runs, &mut writer)?;
    writer.finish()?;

    let mut staged = Vec::with_capacity(5);
    for (name, bytes) in [
        (FILES, encode_files(nonce, &sections.files)),
        (EXTRA, encode_extra(nonce, &sections.extra)),
        (STAMPS, encode_stamps(nonce, &sections.stamps)),
    ] {
        let path = temp(name);
        scratch.0.push(path.clone());
        write_synced(&path, &bytes)?;
        staged.push((path, name));
    }
    staged.push((postings_temp, POSTINGS));
    let meta_temp = temp(META);
    scratch.0.push(meta_temp.clone());
    write_synced(&meta_temp, &encode_meta(nonce, canonical))?;
    staged.push((meta_temp, META));
    for (path, name) in &staged {
        fs::rename(path, dir.join(name))?;
    }
    sync_dir(dir);

    let bytes: Arc<[u8]> = fs::read(dir.join(POSTINGS))?.into();
    let postings = Postings::parse(bytes, nonce)
        .ok_or_else(|| IndexError::Build("published postings failed validation".to_owned()))?;
    Ok(Data {
        files: sections.files,
        extra: sections.extra,
        stamps: sections.stamps,
        postings,
    })
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn sync_dir(dir: &Path) {
    // Directory fsync makes the renames durable where the platform allows it;
    // a platform that refuses it still has the renames in place.
    if let Ok(handle) = File::open(dir) {
        drop(handle.sync_all());
    }
}

pub(super) fn header(magic: [u8; 4], nonce: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN);
    out.extend_from_slice(&magic);
    out.extend_from_slice(&INDEX_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&nonce.to_le_bytes());
    out
}

fn put_path(out: &mut Vec<u8>, path: &Path) {
    let bytes = path.as_os_str().as_encoded_bytes();
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(bytes);
}

fn encode_files(nonce: u64, files: &[PathBuf]) -> Vec<u8> {
    let mut out = header(MAGIC_FILES, nonce);
    out.extend_from_slice(&(files.len() as u64).to_le_bytes());
    for path in files {
        put_path(&mut out, path);
    }
    out
}

fn encode_extra(nonce: u64, extra: &[(PathBuf, ExtraKind)]) -> Vec<u8> {
    let mut out = header(MAGIC_EXTRA, nonce);
    out.extend_from_slice(&(extra.len() as u64).to_le_bytes());
    for (path, kind) in extra {
        out.push(kind.code());
        put_path(&mut out, path);
    }
    out
}

fn encode_stamps(nonce: u64, stamps: &[Option<FileStamp>]) -> Vec<u8> {
    let mut out = header(MAGIC_STAMPS, nonce);
    out.extend_from_slice(&(stamps.len() as u64).to_le_bytes());
    for stamp in stamps {
        let (secs, nanos, size) = stamp.map_or((0, u32::MAX, u64::MAX), |stamp| {
            (stamp.secs, stamp.nanos, stamp.size)
        });
        out.extend_from_slice(&secs.to_le_bytes());
        out.extend_from_slice(&nanos.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
    }
    out
}

fn encode_meta(nonce: u64, canonical: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&INDEX_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&nonce.to_le_bytes());
    out.extend_from_slice(canonical.as_os_str().as_encoded_bytes());
    out
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

fn check_header(bytes: &[u8], magic: [u8; 4], nonce: u64) -> Option<()> {
    let ok = bytes.get(..4)? == magic
        && u32_at(bytes, 4)? == INDEX_FORMAT_VERSION
        && u64_at(bytes, 8)? == nonce;
    ok.then_some(())
}

/// A bounds-checked cursor over one section body.
struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        if len > self.0.len() {
            return None;
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        u32_at(self.take(4)?, 0)
    }

    fn u64(&mut self) -> Option<u64> {
        u64_at(self.take(8)?, 0)
    }

    fn count(&mut self) -> Option<usize> {
        usize::try_from(self.u64()?).ok()
    }

    fn path(&mut self) -> Option<PathBuf> {
        let len = self.count()?;
        Some(bytes_to_path(self.take(len)?))
    }
}

#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

fn section(bytes: &[u8], magic: [u8; 4], nonce: u64) -> Option<(Cursor<'_>, usize)> {
    check_header(bytes, magic, nonce)?;
    let mut cursor = Cursor(bytes.get(HEADER_LEN..)?);
    let count = cursor.count()?;
    // Each record takes at least 8 bytes; a larger count is a corrupt section.
    (count <= cursor.0.len() / 8).then_some((cursor, count))
}

/// Open a published index: header parse, validation, and load only. Any
/// mismatch or read failure is an absent index.
pub(super) fn open(dir: &Path, canonical: &Path) -> Option<Data> {
    let meta = fs::read(dir.join(META)).ok()?;
    let mut cursor = Cursor(&meta);
    if cursor.u32()? != INDEX_FORMAT_VERSION {
        return None;
    }
    let nonce = cursor.u64()?;
    if cursor.0 != canonical.as_os_str().as_encoded_bytes() {
        return None;
    }

    let bytes = fs::read(dir.join(FILES)).ok()?;
    let (mut cursor, count) = section(&bytes, MAGIC_FILES, nonce)?;
    let files = (0..count)
        .map(|_| cursor.path())
        .collect::<Option<Vec<_>>>()?;

    let bytes = fs::read(dir.join(EXTRA)).ok()?;
    let (mut cursor, count) = section(&bytes, MAGIC_EXTRA, nonce)?;
    let extra = (0..count)
        .map(|_| {
            let kind = ExtraKind::from_code(cursor.u8()?)?;
            Some((cursor.path()?, kind))
        })
        .collect::<Option<Vec<_>>>()?;

    let bytes = fs::read(dir.join(STAMPS)).ok()?;
    let (mut cursor, count) = section(&bytes, MAGIC_STAMPS, nonce)?;
    if count != files.len() + extra.len() || cursor.0.len() != count * STAMP_LEN {
        return None;
    }
    let stamps = (0..count)
        .map(|_| {
            let secs = i64::from_le_bytes(cursor.take(8)?.try_into().ok()?);
            let nanos = cursor.u32()?;
            let size = cursor.u64()?;
            Some((nanos != u32::MAX || size != u64::MAX).then_some(FileStamp { secs, nanos, size }))
        })
        .collect::<Option<Vec<_>>>()?;

    let bytes: Arc<[u8]> = fs::read(dir.join(POSTINGS)).ok()?.into();
    let postings = Postings::parse(bytes, nonce)?;
    Some(Data {
        files,
        extra,
        stamps,
        postings,
    })
}
