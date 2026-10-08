//! The index builder: gram extraction into the bounded arena, spill runs,
//! the k-way merge, and the assembly of a full or incremental snapshot.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use super::IndexError;
use super::find::{self, ContentClass, MAX_INDEXABLE_BYTES, RawEntry};
use super::store::{
    Data, ExtraKind, FileStamp, GRAM_LIMIT, MAGIC_POSTINGS, Old, Postings, Sections, header, keep,
    next_bit, next_seq, pack, publish,
};

fn scratch_name(stem: &str) -> String {
    format!("{stem}-{}-{}", std::process::id(), next_seq())
}

/// One posting in the builder arena: 12 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Posting {
    gram: u32,
    file: u32,
    loc: u8,
    next: u8,
}

impl Posting {
    fn key(self) -> u64 {
        (u64::from(self.gram) << 32) | u64::from(self.file)
    }
}

/// The bounded builder arena with its spill runs and per-file gram dedup table.
struct Arena<'d> {
    dir: &'d Path,
    capacity: usize,
    buf: Vec<Posting>,
    spills: Scratch,
    /// Per-gram `loc | next << 8` for the file being extracted.
    dense: Vec<u16>,
    touched: Vec<u32>,
}

/// Scratch files removed on drop, whatever the build outcome.
#[derive(Default)]
pub(super) struct Scratch(pub(super) Vec<PathBuf>);

impl Drop for Scratch {
    fn drop(&mut self) {
        for path in &self.0 {
            // Already-removed scratch is not an error.
            drop(fs::remove_file(path));
        }
    }
}

impl<'d> Arena<'d> {
    fn new(dir: &'d Path, capacity: usize) -> Self {
        Self {
            dir,
            capacity: capacity.max(1),
            buf: Vec::with_capacity(capacity.max(1)),
            spills: Scratch::default(),
            dense: vec![0; GRAM_LIMIT as usize],
            touched: Vec::new(),
        }
    }

    fn add(&mut self, gram: u32, at: usize, next: Option<u8>) {
        let slot = &mut self.dense[gram as usize];
        if *slot == 0 {
            self.touched.push(gram);
        }
        *slot |= u16::from(1_u8 << (at & 7)) | (u16::from(next.map_or(0, next_bit)) << 8);
    }

    /// Extract raw and ASCII-lowercased grams of one file.
    fn extract(&mut self, file: u32, bytes: &[u8]) -> io::Result<()> {
        for (at, window) in bytes.windows(3).enumerate() {
            let next = bytes.get(at + 3).copied();
            self.add(pack(window[0], window[1], window[2]), at, next);
            let [a, b, c] = [window[0], window[1], window[2]].map(|byte| byte.to_ascii_lowercase());
            self.add(
                pack(a, b, c),
                at,
                next.map(|byte| byte.to_ascii_lowercase()),
            );
        }
        let mut touched = std::mem::take(&mut self.touched);
        touched.sort_unstable();
        for gram in touched.drain(..) {
            let slot = &mut self.dense[gram as usize];
            let [loc, next] = slot.to_le_bytes();
            *slot = 0;
            self.push(Posting {
                gram,
                file,
                loc,
                next,
            })?;
        }
        self.touched = touched;
        Ok(())
    }

    pub(super) fn push(&mut self, posting: Posting) -> io::Result<()> {
        if self.buf.len() == self.capacity {
            self.spill()?;
        }
        self.buf.push(posting);
        Ok(())
    }

    /// Write the arena as one sorted delta-varint run beside the index.
    fn spill(&mut self) -> io::Result<()> {
        self.buf.sort_unstable_by_key(|posting| posting.key());
        let path = self.dir.join(format!("{}.spill", scratch_name("build")));
        self.spills.0.push(path.clone());
        let mut out = BufWriter::new(File::create(&path)?);
        put_varint(&mut out, self.buf.len() as u64)?;
        let (mut gram, mut file) = (0, 0);
        for posting in self.buf.drain(..) {
            let gram_delta = posting.gram - gram;
            put_varint(&mut out, u64::from(gram_delta))?;
            let file_value = if gram_delta == 0 {
                posting.file - file
            } else {
                posting.file
            };
            put_varint(&mut out, u64::from(file_value))?;
            out.write_all(&[posting.loc, posting.next])?;
            (gram, file) = (posting.gram, posting.file);
        }
        out.into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()
    }

    /// The sorted runs for the merge: every spill plus the arena remainder.
    fn into_runs(mut self) -> io::Result<(Vec<Run<'static>>, Scratch)> {
        self.buf.sort_unstable_by_key(|posting| posting.key());
        let mut runs = Vec::with_capacity(self.spills.0.len() + 1);
        for path in &self.spills.0 {
            runs.push(Run::Spill(SpillRun::open(path)?));
        }
        let rest = std::mem::take(&mut self.buf);
        runs.push(Run::Memory(rest.into_iter()));
        Ok((runs, std::mem::take(&mut self.spills)))
    }
}

fn put_varint(out: &mut impl Write, mut value: u64) -> io::Result<()> {
    loop {
        let [low, ..] = (value & 0x7F).to_le_bytes();
        value >>= 7;
        if value == 0 {
            return out.write_all(&[low]);
        }
        out.write_all(&[low | 0x80])?;
    }
}

fn get_varint(input: &mut impl Read) -> io::Result<u64> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let mut byte = [0];
        input.read_exact(&mut byte)?;
        value |= u64::from(byte[0] & 0x7F) << shift;
        if byte[0] & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "varint overflow",
    ))
}

fn varint_u32(input: &mut impl Read) -> io::Result<u32> {
    u32::try_from(get_varint(input)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "value out of range"))
}

pub(super) struct SpillRun {
    input: BufReader<File>,
    left: u64,
    gram: u32,
    file: u32,
}

impl SpillRun {
    fn open(path: &Path) -> io::Result<Self> {
        let mut input = BufReader::new(File::open(path)?);
        let left = get_varint(&mut input)?;
        Ok(Self {
            input,
            left,
            gram: 0,
            file: 0,
        })
    }

    fn next(&mut self) -> io::Result<Option<Posting>> {
        if self.left == 0 {
            return Ok(None);
        }
        self.left -= 1;
        let gram_delta = varint_u32(&mut self.input)?;
        let file_value = varint_u32(&mut self.input)?;
        let mut masks = [0; 2];
        self.input.read_exact(&mut masks)?;
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "spill overflow");
        self.gram = self.gram.checked_add(gram_delta).ok_or_else(invalid)?;
        self.file = if gram_delta == 0 {
            self.file.checked_add(file_value).ok_or_else(invalid)?
        } else {
            file_value
        };
        Ok(Some(Posting {
            gram: self.gram,
            file: self.file,
            loc: masks[0],
            next: masks[1],
        }))
    }
}

/// The previous snapshot's postings with file ids remapped; dropped ids vanish.
pub(super) struct OldRun<'a> {
    postings: &'a Postings,
    remap: &'a [Option<u32>],
    gram_at: usize,
    gram: u32,
    at: usize,
    end: usize,
}

impl OldRun<'_> {
    fn next(&mut self) -> Option<Posting> {
        loop {
            while self.at == self.end {
                if self.gram_at == self.postings.grams {
                    return None;
                }
                let (gram, len, start) = self.postings.entry(self.gram_at)?;
                (self.gram, self.at, self.end) = (gram, start, start + len);
                self.gram_at += 1;
            }
            let (file, loc, next) = self.postings.record(self.at)?;
            self.at += 1;
            if let Some(Some(file)) = self.remap.get(file as usize) {
                return Some(Posting {
                    gram: self.gram,
                    file: *file,
                    loc,
                    next,
                });
            }
        }
    }
}

pub(super) enum Run<'a> {
    Memory(std::vec::IntoIter<Posting>),
    Spill(SpillRun),
    Old(OldRun<'a>),
}

impl Run<'_> {
    fn next(&mut self) -> io::Result<Option<Posting>> {
        match self {
            Self::Memory(items) => Ok(items.next()),
            Self::Spill(run) => run.next(),
            Self::Old(run) => Ok(run.next()),
        }
    }
}

/// Streams merged postings into `postings.bin`; the gram table goes to a
/// scratch file and is appended after the postings.
pub(super) struct PostingsWriter {
    out: BufWriter<File>,
    table: BufWriter<File>,
    table_path: PathBuf,
    pending: Option<Posting>,
    open_gram: Option<(u32, u32, u64)>,
    count: u64,
    grams: u64,
}

impl PostingsWriter {
    pub(super) fn create(
        path: &Path,
        dir: &Path,
        nonce: u64,
        scratch: &mut Scratch,
    ) -> io::Result<Self> {
        let table_path = dir.join(format!("{}.spill", scratch_name("build")));
        scratch.0.push(table_path.clone());
        let mut out = BufWriter::new(File::create(path)?);
        out.write_all(&header(MAGIC_POSTINGS, nonce))?;
        Ok(Self {
            out,
            table: BufWriter::new(File::create(&table_path)?),
            table_path,
            pending: None,
            open_gram: None,
            count: 0,
            grams: 0,
        })
    }

    pub(super) fn push(&mut self, posting: Posting) -> io::Result<()> {
        if let Some(pending) = &mut self.pending
            && pending.key() == posting.key()
        {
            pending.loc |= posting.loc;
            pending.next |= posting.next;
            return Ok(());
        }
        self.flush_pending()?;
        self.pending = Some(posting);
        Ok(())
    }

    fn flush_pending(&mut self) -> io::Result<()> {
        let Some(posting) = self.pending.take() else {
            return Ok(());
        };
        if let Some((gram, len, _)) = &mut self.open_gram
            && *gram == posting.gram
        {
            *len += 1;
        } else {
            self.close_gram()?;
            self.open_gram = Some((posting.gram, 1, self.count));
        }
        self.out.write_all(&posting.file.to_le_bytes())?;
        self.out.write_all(&[posting.loc, posting.next])?;
        self.count += 1;
        Ok(())
    }

    fn close_gram(&mut self) -> io::Result<()> {
        if let Some((gram, len, start)) = self.open_gram.take() {
            self.table.write_all(&gram.to_le_bytes())?;
            self.table.write_all(&len.to_le_bytes())?;
            self.table.write_all(&start.to_le_bytes())?;
            self.grams += 1;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> io::Result<()> {
        self.flush_pending()?;
        self.close_gram()?;
        self.table.flush()?;
        drop(self.table);
        io::copy(&mut File::open(&self.table_path)?, &mut self.out)?;
        self.out.write_all(&self.count.to_le_bytes())?;
        self.out.write_all(&self.grams.to_le_bytes())?;
        self.out
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?
            .sync_all()
    }
}

/// K-way merge of sorted runs straight into the postings writer.
pub(super) fn merge(mut runs: Vec<Run<'_>>, writer: &mut PostingsWriter) -> io::Result<()> {
    let mut heads = Vec::with_capacity(runs.len());
    let mut heap = BinaryHeap::with_capacity(runs.len());
    for (at, run) in runs.iter_mut().enumerate() {
        let posting = run.next()?;
        if let Some(posting) = posting {
            heap.push(Reverse((posting.key(), at)));
        }
        heads.push(posting);
    }
    while let Some(Reverse((_, at))) = heap.pop() {
        if let Some(posting) = heads[at].take() {
            writer.push(posting)?;
        }
        heads[at] = runs[at].next()?;
        if let Some(posting) = heads[at] {
            heap.push(Reverse((posting.key(), at)));
        }
    }
    Ok(())
}

/// What one file contributes to a build.
enum Extracted {
    Indexed(Vec<u8>),
    Extra(ExtraKind, bool),
}

/// Read and classify one file; unreadable files are listed but never stamped.
fn read_file(path: &Path, meta: &fs::Metadata) -> Extracted {
    if meta.len() > MAX_INDEXABLE_BYTES {
        return Extracted::Extra(ExtraKind::Oversize, true);
    }
    let Ok(bytes) = fs::read(path) else {
        return Extracted::Extra(ExtraKind::Unreadable, false);
    };
    if bytes.len() as u64 > MAX_INDEXABLE_BYTES {
        return Extracted::Extra(ExtraKind::Oversize, true);
    }
    match find::classify_probe(&bytes) {
        ContentClass::Text => Extracted::Indexed(bytes),
        ContentClass::Binary => Extracted::Extra(ExtraKind::Binary, true),
        ContentClass::Oversize => Extracted::Extra(ExtraKind::Oversize, true),
        ContentClass::Utf16NoBom => Extracted::Extra(ExtraKind::Utf16, true),
    }
}

/// Build and publish a snapshot from a stat walk. With `prev`, unchanged files
/// keep their postings and only changed or new files are read.
pub(super) fn build_dir(
    dir: &Path,
    canonical: &Path,
    prev: Option<&Data>,
    listing: Vec<RawEntry>,
    arena_capacity: usize,
) -> Result<Data, IndexError> {
    let old = prev.map(Data::old_entries).unwrap_or_default();
    let mut remap = vec![None; prev.map_or(0, |data| data.files.len())];
    let mut arena = Arena::new(dir, arena_capacity);
    let mut files = Vec::new();
    let mut file_stamps = Vec::new();
    let mut extra = Vec::new();
    let mut extra_stamps = Vec::new();
    for raw in listing {
        let stamp = FileStamp::of(&raw.meta);
        let id = u32::try_from(files.len())
            .map_err(|_| IndexError::Build("more than 4294967295 indexed files".to_owned()))?;
        match keep(&old, &raw) {
            Some(Old::File(old_id)) => {
                remap[*old_id as usize] = Some(id);
                files.push(raw.path);
                file_stamps.push(stamp);
                continue;
            }
            Some(Old::Extra(kind)) => {
                extra.push((raw.path, *kind));
                extra_stamps.push(stamp.filter(|_| *kind != ExtraKind::Dir));
                continue;
            }
            None if raw.is_dir => {
                extra.push((raw.path, ExtraKind::Dir));
                extra_stamps.push(None);
                continue;
            }
            None => {}
        }
        match read_file(&canonical.join(&raw.path), &raw.meta) {
            Extracted::Indexed(bytes) => {
                arena.extract(id, &bytes)?;
                files.push(raw.path);
                file_stamps.push(stamp);
            }
            Extracted::Extra(kind, stamped) => {
                extra.push((raw.path, kind));
                extra_stamps.push(stamp.filter(|_| stamped));
            }
        }
    }
    let (runs, mut scratch) = arena.into_runs()?;
    let old_run = prev.map(|data| OldRun {
        postings: &data.postings,
        remap: &remap,
        gram_at: 0,
        gram: 0,
        at: 0,
        end: 0,
    });
    let mut all: Vec<Run<'_>> = runs;
    all.extend(old_run.map(Run::Old));
    file_stamps.extend(extra_stamps);
    publish(
        dir,
        canonical,
        Sections {
            files,
            extra,
            stamps: file_stamps,
        },
        all,
        &mut scratch,
    )
}
