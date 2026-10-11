//! Pure collection and rendering of workspace and data-root instructions.

use std::path::{Path, PathBuf};

/// Maximum number of bytes retained from one instruction file before truncation.
pub const MAX_FILE_BYTES: usize = 131_072;
/// Maximum accumulated UTF-8 content bytes, including truncation markers.
pub const MAX_TOTAL_BYTES: usize = 524_288;
/// Text appended to a file whose content exceeds [`MAX_FILE_BYTES`].
pub const TRUNCATION_MARKER: &str = "[truncated at 131072 bytes]";

/// One retained `AGENTS.md` file in root-first order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionFile {
    /// Canonical path supplied to the reader.
    pub path: PathBuf,
    /// Valid UTF-8 file content, followed by [`TRUNCATION_MARKER`] when truncated.
    pub content: Box<str>,
    /// Whether bytes beyond the per-file limit were present.
    pub truncated: bool,
}

/// A concrete failure returned by a host-provided instruction reader.
#[derive(Debug, thiserror::Error)]
#[error("instruction file read failed: {0}")]
pub struct InstructionReadError(#[from] std::io::Error);

/// Reads a bounded prefix of a file at an explicit, canonical path.
pub trait FileReader {
    /// Returns `None` for an absent path, or up to `max_bytes` leading bytes.
    ///
    /// # Errors
    /// Returns the host's I/O failure when the path exists but cannot be read.
    fn read(&self, path: &Path, max_bytes: usize) -> Result<Option<Vec<u8>>, InstructionReadError>;
}

struct ReadContent {
    content: Box<str>,
    truncated: bool,
}

fn log_skipped(path: &Path, message: &str) {
    tracing::info!(target: "dalgon.instructions", path = %path.display(), "{message}");
}

/// Retains the valid UTF-8 prefix when truncation cut a final multi-byte character.
fn truncated_utf8_prefix(bytes: &[u8]) -> Option<&str> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()
        }
        Err(_) => None,
    }
}

fn read_content<R: FileReader>(reader: &R, path: &Path) -> Option<ReadContent> {
    let mut bytes = match reader.read(path, MAX_FILE_BYTES + 1) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return None,
        Err(_) => {
            log_skipped(path, "could not read prompt instruction file");
            return None;
        }
    };

    bytes.truncate(MAX_FILE_BYTES + 1);
    let truncated = bytes.len() > MAX_FILE_BYTES;
    if truncated {
        bytes.truncate(MAX_FILE_BYTES);
    }

    let text = if truncated {
        truncated_utf8_prefix(&bytes)
    } else {
        std::str::from_utf8(&bytes).ok()
    };
    let Some(text) = text else {
        log_skipped(path, "skipping prompt instruction file with invalid UTF-8");
        return None;
    };

    let marker_bytes = if truncated {
        TRUNCATION_MARKER.len()
    } else {
        0
    };
    let mut content = String::with_capacity(text.len() + marker_bytes);
    content.push_str(text);
    if truncated {
        content.push_str(TRUNCATION_MARKER);
    }

    Some(ReadContent {
        content: content.into_boxed_str(),
        truncated,
    })
}

/// Collects `AGENTS.md` files from the root through `cwd`, inclusive.
///
/// If `cwd` is outside `root`, collection starts at the filesystem root of `cwd`.
/// Paths are already canonical and are never resolved by this function. Missing,
/// unreadable, or invalid-UTF-8 files are skipped. The total budget counts UTF-8
/// bytes in retained file content, including truncation markers, but not headings.
pub fn collect<R: FileReader>(reader: &R, root: &Path, cwd: &Path) -> Vec<InstructionFile> {
    let cwd_is_below_root = cwd.starts_with(root);
    let mut directories = cwd
        .ancestors()
        .take_while(|directory| !cwd_is_below_root || directory.starts_with(root))
        .collect::<Vec<_>>();
    directories.reverse();

    let mut files = Vec::new();
    let mut total_bytes = 0;
    for directory in directories {
        let path = directory.join("AGENTS.md");
        let Some(read) = read_content(reader, &path) else {
            continue;
        };
        if read.content.len() > MAX_TOTAL_BYTES.saturating_sub(total_bytes) {
            break;
        }
        total_bytes += read.content.len();
        files.push(InstructionFile {
            path,
            content: read.content,
            truncated: read.truncated,
        });
    }
    files
}

/// Renders retained files in slice order with one blank line between file bodies.
///
/// Returns `None` when `files` is empty.
#[must_use]
pub fn render(files: &[InstructionFile]) -> Option<Box<str>> {
    if files.is_empty() {
        return None;
    }

    let mut rendered = String::new();
    for file in files {
        rendered.push_str("# Instructions from ");
        rendered.push_str(&file.path.to_string_lossy());
        rendered.push('\n');
        rendered.push_str(&file.content);
        if !file.content.ends_with('\n') {
            rendered.push('\n');
        }
        rendered.push('\n');
    }
    Some(rendered.into_boxed_str())
}

/// Loads raw `SYSTEM.md` content from the explicit data root only.
///
/// Files use the same per-file limit and UTF-8 rules as workspace instructions.
/// Returns `None` when the file is absent, unreadable, invalid, or empty.
pub fn load_system_md<R: FileReader>(reader: &R, data_root: &Path) -> Option<Box<str>> {
    let path = data_root.join("SYSTEM.md");
    let read = read_content(reader, &path)?;
    if read.content.is_empty() {
        None
    } else {
        Some(read.content)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FileReader, InstructionFile, InstructionReadError, MAX_FILE_BYTES, MAX_TOTAL_BYTES,
        TRUNCATION_MARKER, collect, load_system_md, render,
    };
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};

    enum Entry {
        Bytes(Vec<u8>),
        Unreadable,
    }

    #[derive(Default)]
    struct MemoryReader {
        files: HashMap<PathBuf, Entry>,
        reads: RefCell<Vec<(PathBuf, usize)>>,
    }

    impl MemoryReader {
        fn insert(&mut self, path: impl Into<PathBuf>, bytes: Vec<u8>) {
            self.files.insert(path.into(), Entry::Bytes(bytes));
        }

        fn unreadable(&mut self, path: impl Into<PathBuf>) {
            self.files.insert(path.into(), Entry::Unreadable);
        }
    }

    fn read_denied() -> InstructionReadError {
        std::io::Error::new(ErrorKind::PermissionDenied, "fixture read denied").into()
    }

    fn entry_bytes(
        entry: Option<&Entry>,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, InstructionReadError> {
        match entry {
            Some(Entry::Bytes(bytes)) => Ok(Some(bytes.iter().take(max_bytes).copied().collect())),
            Some(Entry::Unreadable) => Err(read_denied()),
            None => Ok(None),
        }
    }

    impl FileReader for MemoryReader {
        fn read(
            &self,
            path: &Path,
            max_bytes: usize,
        ) -> Result<Option<Vec<u8>>, InstructionReadError> {
            self.reads
                .borrow_mut()
                .push((path.to_path_buf(), max_bytes));
            entry_bytes(self.files.get(path), max_bytes)
        }
    }

    fn agents(directory: &Path) -> PathBuf {
        directory.join("AGENTS.md")
    }

    #[test]
    fn collect_walks_from_root_to_cwd_inclusive() {
        let root = PathBuf::from("/workspace");
        let project = root.join("project");
        let cwd = project.join("nested");
        let mut reader = MemoryReader::default();
        for directory in [&root, &project, &cwd] {
            reader.insert(
                agents(directory),
                directory.to_string_lossy().as_bytes().to_vec(),
            );
        }

        let files = collect(&reader, &root, &cwd);
        let expected = [agents(&root), agents(&project), agents(&cwd)];
        let actual = files.iter().map(|file| &file.path).collect::<Vec<_>>();
        assert_eq!(actual, expected.iter().collect::<Vec<_>>());
    }

    #[test]
    fn collect_at_root_reads_only_that_directory() {
        let root = PathBuf::from("/workspace");
        let child = root.join("child");
        let mut reader = MemoryReader::default();
        reader.insert(agents(&root), b"root".to_vec());
        reader.insert(agents(&child), b"child".to_vec());

        let files = collect(&reader, &root, &root);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, agents(&root));
    }

    #[test]
    fn collect_outside_root_starts_at_cwds_filesystem_root() {
        let root = PathBuf::from("/workspace");
        let cwd = PathBuf::from("/other/deep");
        let mut reader = MemoryReader::default();
        reader.insert(agents(Path::new("/")), b"filesystem".to_vec());
        reader.insert(agents(Path::new("/other")), b"other".to_vec());
        reader.insert(agents(&cwd), b"deep".to_vec());
        reader.insert(agents(&root), b"workspace".to_vec());

        let files = collect(&reader, &root, &cwd);
        let expected = [
            agents(Path::new("/")),
            agents(Path::new("/other")),
            agents(&cwd),
        ];
        let actual = files.iter().map(|file| &file.path).collect::<Vec<_>>();
        assert_eq!(actual, expected.iter().collect::<Vec<_>>());
    }

    #[test]
    fn collect_keeps_an_exact_file_limit_without_a_marker() {
        let root = PathBuf::from("/workspace");
        let mut reader = MemoryReader::default();
        reader.insert(agents(&root), vec![b'x'; MAX_FILE_BYTES]);

        let files = collect(&reader, &root, &root);
        assert_eq!(files[0].content.len(), MAX_FILE_BYTES);
        assert!(!files[0].truncated);
        assert_eq!(reader.reads.borrow()[0].1, MAX_FILE_BYTES + 1);
    }

    #[test]
    fn collect_marks_an_oversized_file_after_a_valid_prefix() {
        let root = PathBuf::from("/workspace");
        let mut reader = MemoryReader::default();
        reader.insert(agents(&root), vec![b'x'; MAX_FILE_BYTES + 1]);

        let files = collect(&reader, &root, &root);
        assert!(files[0].truncated);
        assert_eq!(
            files[0].content.as_ref(),
            format!("{}{}", "x".repeat(MAX_FILE_BYTES), TRUNCATION_MARKER)
        );
    }

    #[test]
    fn collect_keeps_only_the_valid_prefix_when_a_utf8_character_crosses_the_cap() {
        let root = PathBuf::from("/workspace");
        let mut bytes = vec![b'a'; MAX_FILE_BYTES - 1];
        bytes.extend_from_slice(&[0xF0, 0x9F, 0x8C, 0x9F, b'x']);
        let mut reader = MemoryReader::default();
        reader.insert(agents(&root), bytes);

        let files = collect(&reader, &root, &root);
        assert_eq!(
            files[0].content.as_ref(),
            format!("{}{}", "a".repeat(MAX_FILE_BYTES - 1), TRUNCATION_MARKER)
        );
        assert!(files[0].truncated);
    }

    #[test]
    fn collect_skips_invalid_utf8_away_from_a_truncation_boundary() {
        let root = PathBuf::from("/workspace");
        let mut reader = MemoryReader::default();
        reader.insert(agents(&root), b"valid\xFFinvalid".to_vec());

        assert_eq!(collect(&reader, &root, &root), []);
    }

    #[test]
    fn collect_skips_invalid_utf8_inside_an_oversized_files_prefix() {
        let root = PathBuf::from("/workspace");
        let mut bytes = vec![b'x'; MAX_FILE_BYTES + 1];
        bytes[MAX_FILE_BYTES / 2] = 0xFF;
        let mut reader = MemoryReader::default();
        reader.insert(agents(&root), bytes);

        assert_eq!(collect(&reader, &root, &root), []);
    }

    #[test]
    fn collect_skips_a_file_that_would_exceed_the_total_content_cap() {
        let directories = [
            PathBuf::from("/workspace"),
            PathBuf::from("/workspace/a"),
            PathBuf::from("/workspace/a/b"),
            PathBuf::from("/workspace/a/b/c"),
            PathBuf::from("/workspace/a/b/c/d"),
        ];
        let mut reader = MemoryReader::default();
        for directory in &directories {
            reader.insert(agents(directory), vec![b'x'; MAX_FILE_BYTES]);
        }

        let files = collect(&reader, &directories[0], &directories[4]);
        assert_eq!(files.len(), 4);
        assert_eq!(
            files.iter().map(|file| file.content.len()).sum::<usize>(),
            MAX_TOTAL_BYTES
        );
    }

    #[test]
    fn collect_counts_truncation_markers_and_never_splits_a_file_for_the_total_cap() {
        let directories = [
            PathBuf::from("/workspace"),
            PathBuf::from("/workspace/a"),
            PathBuf::from("/workspace/a/b"),
            PathBuf::from("/workspace/a/b/c"),
        ];
        let mut reader = MemoryReader::default();
        for directory in &directories[..3] {
            reader.insert(agents(directory), vec![b'x'; MAX_FILE_BYTES]);
        }
        reader.insert(agents(&directories[3]), vec![b'y'; MAX_FILE_BYTES + 1]);

        let files = collect(&reader, &directories[0], &directories[3]);
        assert_eq!(files.len(), 3);
        assert_eq!(
            files.iter().map(|file| file.content.len()).sum::<usize>(),
            MAX_TOTAL_BYTES - MAX_FILE_BYTES
        );
    }

    #[test]
    fn collect_skips_absent_and_unreadable_files_without_stopping_the_walk() {
        let root = PathBuf::from("/workspace");
        let missing = root.join("missing");
        let unreadable = missing.join("unreadable");
        let cwd = unreadable.join("deep");
        let mut reader = MemoryReader::default();
        reader.insert(agents(&root), b"root".to_vec());
        reader.unreadable(agents(&unreadable));
        reader.insert(agents(&cwd), b"deep".to_vec());

        let files = collect(&reader, &root, &cwd);
        let bodies = files
            .iter()
            .map(|file| file.content.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(bodies, vec!["root", "deep"]);
    }

    #[test]
    fn render_preserves_root_first_order_and_exact_blank_lines() {
        let root_file = InstructionFile {
            path: PathBuf::from("/workspace/AGENTS.md"),
            content: "root".into(),
            truncated: false,
        };
        let child_file = InstructionFile {
            path: PathBuf::from("/workspace/child/AGENTS.md"),
            content: "child\n".into(),
            truncated: false,
        };

        let rendered = render(&[root_file, child_file]);
        let expected = "# Instructions from /workspace/AGENTS.md\nroot\n\n# Instructions from /workspace/child/AGENTS.md\nchild\n\n";
        assert_eq!(rendered.as_deref(), Some(expected));
    }

    #[test]
    fn render_returns_none_for_an_empty_list() {
        assert_eq!(render(&[]), None);
    }

    #[test]
    fn load_system_md_returns_unlabelled_raw_content_from_only_the_data_root() {
        let data_root = PathBuf::from("/data");
        let mut reader = MemoryReader::default();
        reader.insert(
            data_root.join("SYSTEM.md"),
            b"  exact\nraw system text \n".to_vec(),
        );
        reader.insert(
            data_root.join("workspace/SYSTEM.md"),
            b"must not be loaded".to_vec(),
        );

        assert_eq!(
            load_system_md(&reader, &data_root).as_deref(),
            Some("  exact\nraw system text \n")
        );
        assert_eq!(reader.reads.borrow().len(), 1);
    }

    #[test]
    fn load_system_md_returns_none_for_absent_empty_unreadable_or_invalid_content() {
        let data_root = PathBuf::from("/data");
        let system_md = data_root.join("SYSTEM.md");
        let mut reader = MemoryReader::default();
        assert_eq!(load_system_md(&reader, &data_root), None);

        reader.insert(&system_md, Vec::new());
        assert_eq!(load_system_md(&reader, &data_root), None);

        reader.unreadable(&system_md);
        assert_eq!(load_system_md(&reader, &data_root), None);

        reader.insert(&system_md, b"invalid\xFFutf8".to_vec());
        assert_eq!(load_system_md(&reader, &data_root), None);
    }
}
