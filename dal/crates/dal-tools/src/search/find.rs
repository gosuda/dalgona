//! The search file universe: one `ignore` walk per workspace and the find mode.
//!
//! The walk includes hidden files, never follows symbolic links, skips `.git`,
//! and honors nested `.gitignore`, `.git/info/exclude`, and the global gitignore.
//! A directory that the rules exclude hides all of its children, even children
//! that a deeper rule re-includes. Paths sort by raw path bytes.

use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use globset::{GlobBuilder, GlobMatcher};
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};

use crate::search::SearchError;

/// Bytes read from the head of a file to classify its content.
pub(crate) const PROBE_BYTES: usize = 8192;
const PROBE_BYTES_U64: u64 = 8192;
/// Files larger than this are listable but never searched or indexed.
pub(crate) const MAX_INDEXABLE_BYTES: u64 = 16 << 20;

/// One listable path of the search universe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Path relative to the walk root.
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
    /// False for NUL-bearing, UTF-16 without BOM, oversize, and unreadable files.
    pub(crate) indexable: bool,
}

/// The content class of one regular file, decided from its size and first 8192 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentClass {
    Text,
    /// A NUL byte in the probe that is not the UTF-16 pattern.
    Binary,
    /// Larger than 16 MiB.
    Oversize,
    /// NUL bytes at one byte parity only: UTF-16 without a byte order mark.
    Utf16NoBom,
}

impl ContentClass {
    pub(crate) fn indexable(self) -> bool {
        self == Self::Text
    }
}

/// A compiled find glob. Compile once per query.
#[derive(Debug, Clone)]
pub(crate) struct FindGlob {
    matcher: GlobMatcher,
    /// A glob without `/` matches the file name at any depth.
    basename: bool,
}

impl FindGlob {
    pub(crate) fn new(pattern: &str) -> Result<Self, SearchError> {
        let basename = !pattern.contains('/');
        let anchored = pattern.strip_prefix('/').unwrap_or(pattern);
        let glob = GlobBuilder::new(anchored)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| SearchError::InvalidGlob {
                pattern: pattern.to_owned(),
                reason: error.kind().to_string(),
            })?;
        Ok(Self {
            matcher: glob.compile_matcher(),
            basename,
        })
    }

    /// Match a path relative to the search root.
    pub(crate) fn is_match(&self, relative: &Path) -> bool {
        if self.basename {
            relative
                .file_name()
                .is_some_and(|name| self.matcher.is_match(name))
        } else {
            self.matcher.is_match(relative)
        }
    }
}

/// The ordered, capped result of a find query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FindResult {
    /// Display rows: directories first with a trailing `/`, then files; bytewise in each group.
    pub(crate) paths: Vec<String>,
    /// How many leading rows of `paths` are directories.
    pub(crate) dirs: usize,
    /// Every match before the `limit` cut.
    pub(crate) total: usize,
}

impl FindResult {
    /// The exact tool text for this result.
    pub(crate) fn render(&self, pattern: &str) -> String {
        if self.total == 0 {
            return format!("No files match {pattern}");
        }
        let mut text = self.paths.join("\n");
        let more = self.total - self.paths.len();
        if more > 0 {
            text.push_str(&format!("\n[Truncated: {more} more paths]"));
        }
        text
    }
}

/// Walk `root` once and classify every regular file. Paths are relative to `root`.
pub(crate) fn walk(root: &Path) -> Result<Vec<Entry>, SearchError> {
    Ok(walk_probed(root)?
        .into_iter()
        .map(|raw| Entry {
            indexable: raw.class.is_some_and(ContentClass::indexable),
            path: raw.path,
            is_dir: raw.is_dir,
        })
        .collect())
}

/// Find paths under `root` whose relative path matches `pattern`.
pub(crate) fn find(root: &Path, pattern: &str, limit: usize) -> Result<FindResult, SearchError> {
    find_with(root, &FindGlob::new(pattern)?, limit)
}

/// Find with an already compiled glob; the full-walk path of find.
pub(crate) fn find_with(
    root: &Path,
    glob: &FindGlob,
    limit: usize,
) -> Result<FindResult, SearchError> {
    let entries = walk_listing(root)?;
    Ok(select(
        entries.iter().map(|raw| (raw.path.as_path(), raw.is_dir)),
        glob,
        limit,
    ))
}

/// Match, order, and cap listable paths. Shared by the walk and the index fast path.
pub(crate) fn select<'a>(
    items: impl Iterator<Item = (&'a Path, bool)>,
    glob: &FindGlob,
    limit: usize,
) -> FindResult {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for (path, is_dir) in items {
        if glob.is_match(path) {
            (if is_dir { &mut dirs } else { &mut files }).push(path);
        }
    }
    dirs.sort_unstable_by(|a, b| path_bytes_cmp(a, b));
    files.sort_unstable_by(|a, b| path_bytes_cmp(a, b));
    let total = dirs.len() + files.len();
    let kept_dirs = dirs.len().min(limit);
    let paths = dirs
        .into_iter()
        .map(|path| format!("{}/", display(path)))
        .chain(files.into_iter().map(display))
        .take(limit)
        .collect();
    FindResult {
        paths,
        dirs: kept_dirs,
        total,
    }
}

/// Classify one file from its size and first 8192 bytes.
pub(crate) fn content_class(path: &Path) -> io::Result<ContentClass> {
    let file = File::open(path)?;
    if file.metadata()?.len() > MAX_INDEXABLE_BYTES {
        return Ok(ContentClass::Oversize);
    }
    let mut probe = Vec::with_capacity(PROBE_BYTES);
    file.take(PROBE_BYTES_U64).read_to_end(&mut probe)?;
    Ok(classify_probe(&probe))
}

/// Classify the head bytes of a file that is at most 16 MiB long.
pub(crate) fn classify_probe(probe: &[u8]) -> ContentClass {
    let probe = &probe[..probe.len().min(PROBE_BYTES)];
    let mut nuls = [0_usize; 2];
    for (at, _) in probe.iter().enumerate().filter(|(_, byte)| **byte == 0) {
        nuls[at % 2] += 1;
    }
    if nuls == [0, 0] {
        return ContentClass::Text;
    }
    let has_bom = probe.starts_with(&[0xFF, 0xFE]) || probe.starts_with(&[0xFE, 0xFF]);
    // The plan's rule: a NUL pattern that alternates at one byte parity over
    // the probe is UTF-16 without a BOM; anything else with NULs is binary.
    let one_parity = nuls[0] == 0 || nuls[1] == 0;
    if !has_bom && one_parity {
        ContentClass::Utf16NoBom
    } else {
        ContentClass::Binary
    }
}

/// One walked path with its `lstat` metadata and, when probed, its content class.
#[derive(Debug)]
pub(crate) struct RawEntry {
    /// Path relative to the walk root.
    pub(crate) path: PathBuf,
    pub(crate) is_dir: bool,
    pub(crate) meta: Metadata,
    /// `None` for directories, for unprobed walks, and for unreadable files.
    pub(crate) class: Option<ContentClass>,
}

/// The universe walk that reads no file content: names and `lstat` metadata only.
/// This is the stat walk of the index freshness check and the listing of find.
pub(crate) fn walk_listing(root: &Path) -> Result<Vec<RawEntry>, SearchError> {
    walk_universe(root, Probe::Skip)
}

/// The universe walk that also classifies every regular file from its head bytes.
pub(crate) fn walk_probed(root: &Path) -> Result<Vec<RawEntry>, SearchError> {
    walk_universe(root, Probe::Classify)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Probe {
    Skip,
    Classify,
}

fn walk_universe(root: &Path, probe: Probe) -> Result<Vec<RawEntry>, SearchError> {
    let root_meta = std::fs::metadata(root).map_err(|error| SearchError::Walk {
        path: root.to_path_buf(),
        reason: error.to_string(),
    })?;
    if !root_meta.is_dir() {
        return Err(SearchError::Walk {
            path: root.to_path_buf(),
            reason: "not a directory".to_owned(),
        });
    }
    let sink = Mutex::new(None);
    let root_error = Mutex::new(None);
    let mut builder = Collectors {
        root,
        probe,
        sink: &sink,
        root_error: &root_error,
    };
    WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .parents(true)
        .ignore(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .require_git(false)
        .current_dir(root)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build_parallel()
        .visit(&mut builder);
    if let Some(reason) = take(&root_error) {
        return Err(SearchError::Walk {
            path: root.to_path_buf(),
            reason,
        });
    }
    let mut entries = take(&sink).unwrap_or_default();
    entries.sort_unstable_by(|a: &RawEntry, b: &RawEntry| path_bytes_cmp(&a.path, &b.path));
    Ok(entries)
}

fn take<T>(cell: &Mutex<Option<T>>) -> Option<T> {
    match cell.lock() {
        Ok(mut guard) => guard.take(),
        Err(poisoned) => poisoned.into_inner().take(),
    }
}

/// Byte order of raw path bytes, the one sort order of every search listing.
pub(crate) fn path_bytes_cmp(a: &Path, b: &Path) -> std::cmp::Ordering {
    a.as_os_str()
        .as_encoded_bytes()
        .cmp(b.as_os_str().as_encoded_bytes())
}

/// A display row for a relative path, with `/` separators.
pub(crate) fn display(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.into_owned()
    }
}

struct Collectors<'s> {
    root: &'s Path,
    probe: Probe,
    sink: &'s Mutex<Option<Vec<RawEntry>>>,
    root_error: &'s Mutex<Option<String>>,
}

impl<'s> ParallelVisitorBuilder<'s> for Collectors<'s> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Collector {
            root: self.root,
            probe: self.probe,
            sink: self.sink,
            root_error: self.root_error,
            found: Vec::new(),
        })
    }
}

struct Collector<'s> {
    root: &'s Path,
    probe: Probe,
    sink: &'s Mutex<Option<Vec<RawEntry>>>,
    root_error: &'s Mutex<Option<String>>,
    found: Vec<RawEntry>,
}

impl Collector<'_> {
    fn admit(&self, entry: &DirEntry) -> Option<RawEntry> {
        if entry.depth() == 0 {
            return None;
        }
        let file_type = entry.file_type()?;
        let is_dir = file_type.is_dir();
        if !is_dir && !file_type.is_file() {
            // Symbolic links, sockets, FIFOs, and devices are outside the universe.
            return None;
        }
        let meta = entry.metadata().ok()?;
        let path = entry.path().strip_prefix(self.root).ok()?.to_path_buf();
        let class = if self.probe == Probe::Classify && !is_dir {
            content_class(entry.path()).ok()
        } else {
            None
        };
        Some(RawEntry {
            path,
            is_dir,
            meta,
            class,
        })
    }
}

impl ParallelVisitor for Collector<'_> {
    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        match entry {
            Ok(entry) => {
                if let Some(raw) = self.admit(&entry) {
                    self.found.push(raw);
                }
                WalkState::Continue
            }
            Err(error) if error.depth() == Some(0) => {
                if let Ok(mut slot) = self.root_error.lock() {
                    slot.get_or_insert_with(|| error.to_string());
                }
                WalkState::Quit
            }
            // An unreadable subdirectory is skipped; the rest of the universe stands.
            Err(_) => WalkState::Continue,
        }
    }
}

impl Drop for Collector<'_> {
    fn drop(&mut self) {
        let found = std::mem::take(&mut self.found);
        let mut guard = match self.sink.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.get_or_insert_with(Vec::new).extend(found);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use super::*;

    fn put(root: &Path, relative: &str, bytes: &[u8]) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    /// An ignore-adversarial corpus: nested rules, negations under excluded
    /// directories, `.git/info/exclude`, hidden files, binaries, and symlinks.
    fn corpus(root: &Path) {
        fs::create_dir_all(root.join(".git/info")).unwrap();
        fs::write(root.join(".git/info/exclude"), "excluded_by_info.rs\n").unwrap();
        put(root, ".git/HEAD.rs", b"ref");
        put(
            root,
            ".gitignore",
            b"vendored/\n*.gen.rs\n/top_only.rs\nartifacts\n!keep.gen.rs\n",
        );
        put(root, "src/main.rs", b"fn main() {}\n");
        put(root, "src/lib.rs", b"pub fn a() {}\n");
        put(root, "src/.hidden.rs", b"// hidden\n");
        put(root, "src/x.gen.rs", b"gen");
        put(root, "src/keep.gen.rs", b"kept");
        put(root, "top_only.rs", b"root anchored");
        put(root, "nested/top_only.rs", b"not anchored here");
        put(root, "excluded_by_info.rs", b"info");
        put(root, "vendored/debug/out.rs", b"built");
        put(root, "artifacts/x.rs", b"built");
        put(root, "nested/.gitignore", b"local.rs\n!/reinc/\n");
        put(root, "nested/local.rs", b"local");
        put(root, "nested/deeper/local.rs", b"deeper local");
        put(root, "nested/deeper/ok.rs", b"ok");
        // A child re-included under an excluded directory stays excluded.
        put(root, "vendored/.gitignore", b"!inner.rs\n");
        put(root, "vendored/inner.rs", b"reinc");
        put(root, "data/blob.rs", b"a\0b\0\0c");
        put(root, "data/wide.rs", &[b'h', 0, b'i', 0, b'\n', 0]);
        put(root, "dir.rs/child.txt", b"a dir named like a file");
        put(root, "A.rs", b"upper");
        put(root, "a b.rs", b"space");
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("src/main.rs"), root.join("link.rs")).unwrap();
    }

    /// An independent reference: a plain recursive walk that applies the same
    /// rules by hand for this corpus.
    fn reference(root: &Path, dir: &Path, out: &mut Vec<(String, bool)>) {
        let excluded = |rel: &str, is_dir: bool| -> bool {
            let name = rel.rsplit('/').next().unwrap();
            name == ".git"
                || rel == "excluded_by_info.rs"
                || rel == "top_only.rs"
                || (is_dir && (name == "vendored" || name == "artifacts"))
                || (name.ends_with(".gen.rs") && name != "keep.gen.rs")
                || (!is_dir && rel.starts_with("nested/") && name == "local.rs")
        };
        for item in fs::read_dir(dir).unwrap() {
            let item = item.unwrap();
            let kind = item.file_type().unwrap();
            if kind.is_symlink() {
                continue;
            }
            let rel = item
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if excluded(&rel, kind.is_dir()) {
                continue;
            }
            out.push((rel, kind.is_dir()));
            if kind.is_dir() {
                reference(root, &item.path(), out);
            }
        }
    }

    #[test]
    fn find_conformance() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        corpus(root);
        let glob = FindGlob::new("*.rs").unwrap();
        let got = find(root, "*.rs", 1000).unwrap();

        let mut universe = Vec::new();
        reference(root, root, &mut universe);
        let mut dirs: Vec<&str> = Vec::new();
        let mut files: Vec<&str> = Vec::new();
        for (rel, is_dir) in &universe {
            if glob.is_match(Path::new(rel)) {
                (if *is_dir { &mut dirs } else { &mut files }).push(rel);
            }
        }
        dirs.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        files.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        let expected: Vec<String> = dirs
            .iter()
            .map(|d| format!("{d}/"))
            .chain(files.iter().map(|f| (*f).to_owned()))
            .collect();

        let got_set: BTreeSet<&String> = got.paths.iter().collect();
        let want_set: BTreeSet<&String> = expected.iter().collect();
        assert_eq!(got_set.difference(&want_set).count(), 0, "extra: {got:?}");
        assert_eq!(want_set.difference(&got_set).count(), 0, "missing: {got:?}");
        assert_eq!(got.paths, expected, "order");
        assert_eq!(got.total, expected.len());
        assert!(got.paths.contains(&"src/keep.gen.rs".to_owned()));
        assert!(got.paths.contains(&"data/blob.rs".to_owned()));
        assert!(got.paths.contains(&"data/wide.rs".to_owned()));
        assert!(!got.paths.iter().any(|p| p.starts_with("vendored")));
    }

    #[test]
    fn walk_marks_indexability_and_excludes_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        corpus(root);
        let entries = walk(root).unwrap();
        let get = |rel: &str| entries.iter().find(|e| e.path == Path::new(rel)).cloned();
        assert!(get(".git").is_none());
        assert!(get(".git/HEAD.rs").is_none());
        assert!(!get("data/blob.rs").unwrap().indexable);
        assert!(!get("data/wide.rs").unwrap().indexable);
        assert!(get("src/main.rs").unwrap().indexable);
        assert!(get("src").unwrap().is_dir);
        assert!(get("link.rs").is_none());
        let bytes: Vec<&[u8]> = entries
            .iter()
            .map(|e| e.path.as_os_str().as_encoded_bytes())
            .collect();
        assert!(bytes.windows(2).all(|w| w[0] < w[1]), "byte order");
        assert_eq!(
            content_class(&root.join("data/wide.rs")).unwrap(),
            ContentClass::Utf16NoBom
        );
        assert_eq!(
            content_class(&root.join("data/blob.rs")).unwrap(),
            ContentClass::Binary
        );
    }

    #[test]
    fn oversize_is_listed_not_indexable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let big = File::create(root.join("big.txt")).unwrap();
        big.set_len(MAX_INDEXABLE_BYTES + 1).unwrap();
        let entries = walk(root).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].indexable);
        assert_eq!(find(root, "big.txt", 10).unwrap().paths, ["big.txt"]);
    }

    #[test]
    fn nested_gitignore_without_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        put(root, "a/.gitignore", b"skip.txt\n");
        put(root, "a/skip.txt", b"x");
        put(root, "a/keep.txt", b"x");
        let got = find(root, "*.txt", 10).unwrap();
        assert_eq!(got.paths, ["a/keep.txt"]);
    }

    #[test]
    fn find_glob_forms_and_texts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in ["a/x.rs", "a/b/y.rs", "z.rs", "a/b/w.txt"] {
            put(root, name, b"x");
        }
        assert_eq!(find(root, "a/*.rs", 10).unwrap().paths, ["a/x.rs"]);
        assert_eq!(
            find(root, "/a/**/*.rs", 10).unwrap().paths,
            ["a/b/y.rs", "a/x.rs"]
        );
        assert_eq!(find(root, "b", 10).unwrap().paths, ["a/b/"]);
        let capped = find(root, "*.rs", 2).unwrap();
        assert_eq!(capped.paths, ["a/b/y.rs", "a/x.rs"]);
        assert_eq!(
            capped.render("*.rs"),
            "a/b/y.rs\na/x.rs\n[Truncated: 1 more paths]"
        );
        let none = find(root, "*.md", 10).unwrap();
        assert_eq!(none.render("*.md"), "No files match *.md");
        let error = find(root, "a[", 10).unwrap_err();
        assert!(
            error.to_string().starts_with("search: invalid glob a[: "),
            "{error}"
        );
    }

    #[test]
    fn utf16_probe_rules() {
        assert_eq!(classify_probe(b"plain"), ContentClass::Text);
        assert_eq!(
            classify_probe(&[0, b'a', 0, b'b']),
            ContentClass::Utf16NoBom
        );
        assert_eq!(classify_probe(&[0, 0, 0, 0]), ContentClass::Binary);
        assert_eq!(classify_probe(&[0xFF, 0xFE, b'a', 0]), ContentClass::Binary);
        assert_eq!(classify_probe(&[0, 0, b'a', b'b']), ContentClass::Binary);
        assert_eq!(
            classify_probe(b"abcdefgh\0"),
            ContentClass::Utf16NoBom,
            "the plan's rule is the parity pattern alone"
        );
    }
}
