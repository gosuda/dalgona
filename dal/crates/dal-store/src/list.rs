//! Read-only session listing, resume lookup, and per-store metadata caching.

use std::{
    collections::HashMap,
    fmt,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::SystemTime,
};

use dal_core::{
    BlobId, Entry, EntryKind, JournalPart, ListQuery, Page, Record, Seq, SessionId, SessionInfo,
    Workspace,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::StoreError,
    util::{self, FileMode},
};

const CACHE_VERSION: u8 = 1;
const DEFAULT_LIST_LIMIT: u32 = 200;
const DAMAGED_PREVIEW: &str = "(damaged session file)";
const MAX_PREVIEW_BYTES: usize = 4_096;

/// Store-instance metadata cache and read-only session lookup.
#[derive(Default)]
pub(crate) struct Listing {
    cache: Mutex<HashMap<PathBuf, CachedInfo>>,
}

struct CachedInfo {
    journal_bytes: u64,
    journal_mtime: SystemTime,
    facts: Facts,
}

#[derive(Clone)]
struct Facts {
    id: SessionId,
    workspace: Workspace,
    name: Option<Box<str>>,
    preview: Box<str>,
    created_at: Option<jiff::Timestamp>,
    archived: Option<bool>,
    last_seq: Option<Seq>,
}

/// The deliberately small on-disk cache. Keep declaration order in sync with the format.
#[derive(Deserialize, Serialize)]
struct InfoFile {
    v: u8,
    id: SessionId,
    workspace: Box<str>,
    name: Option<Box<str>>,
    preview: Box<str>,
    #[serde(serialize_with = "serialize_timestamp_millis")]
    created_at: jiff::Timestamp,
    archived: bool,
    journal_bytes: u64,
}

struct Millis<'a>(&'a jiff::Timestamp);

impl fmt::Display for Millis<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:.3}", self.0)
    }
}

fn serialize_timestamp_millis<S>(
    timestamp: &jiff::Timestamp,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.collect_str(&Millis(timestamp))
}

#[derive(Clone)]
struct Listed {
    info: SessionInfo,
    mtime_ms: i64,
    id_text: Box<str>,
}

struct Candidate {
    id: SessionId,
    directory: PathBuf,
    journal_bytes: u64,
    journal_mtime: SystemTime,
    mtime_ms: i64,
    id_text: Box<str>,
}

impl Listing {
    /// Creates an empty cache owned by one store instance.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Lists sessions from one workspace directory, newest first.
    ///
    /// # Errors
    /// Returns `StoreError::ListLimit`, `StoreError::MalformedCursor`, or a filesystem error.
    pub(crate) fn list(
        &self,
        workspace_dir: &Path,
        workspace: &Workspace,
        query: &ListQuery,
    ) -> Result<Page<SessionInfo, Box<str>>, StoreError> {
        let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
        if !(1..=500).contains(&limit) {
            return Err(StoreError::ListLimit);
        }
        let limit = usize::try_from(limit).map_err(|_| StoreError::ListLimit)?;
        let cursor = query.cursor.as_deref().map(parse_cursor).transpose()?;
        let Some(search) = query.search.as_deref() else {
            let mut candidates = Self::read_candidates(workspace_dir)?;
            if let Some(cursor) = cursor {
                candidates.retain(|candidate| {
                    (candidate.mtime_ms, candidate.id_text.as_ref()) < (cursor.0, cursor.1.as_str())
                });
            }
            let more = candidates.len() > limit;
            candidates.truncate(limit);
            let next_before = if more {
                candidates.last().map(|candidate| {
                    format!("{}:{}", candidate.mtime_ms, candidate.id).into_boxed_str()
                })
            } else {
                None
            };
            let items = candidates
                .into_iter()
                .map(|candidate| {
                    self.read_candidate(candidate, workspace)
                        .map(|row| row.info)
                })
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(Page { items, next_before });
        };
        let search = search.to_ascii_lowercase();
        let mut sessions = self.read_workspace(workspace_dir, workspace)?;
        sessions.retain(|session| {
            session
                .info
                .name
                .as_deref()
                .is_some_and(|name| name.to_ascii_lowercase().contains(&search))
                || session.info.preview.to_ascii_lowercase().contains(&search)
        });
        if let Some(cursor) = cursor {
            sessions.retain(|session| {
                (session.mtime_ms, session.id_text.as_ref()) < (cursor.0, cursor.1.as_str())
            });
        }
        let more = sessions.len() > limit;
        sessions.truncate(limit);
        let next_before = if more {
            sessions
                .last()
                .map(|session| format!("{}:{}", session.mtime_ms, session.info.id).into_boxed_str())
        } else {
            None
        };
        Ok(Page {
            items: sessions.into_iter().map(|session| session.info).collect(),
            next_before,
        })
    }

    /// Resolves a full id, exact name, or lowercase id prefix in this workspace.
    ///
    /// # Errors
    /// Returns the corresponding typed `StoreError` when the reference is empty, absent,
    /// ambiguous, or the workspace cannot be read.
    pub(crate) fn resolve(
        &self,
        workspace_dir: &Path,
        workspace: &Workspace,
        arg: &str,
    ) -> Result<SessionId, StoreError> {
        let arg = arg.trim();
        if arg.is_empty() {
            return Err(StoreError::EmptyRef);
        }

        if let Ok(id) = SessionId::parse(arg) {
            let journal = workspace_dir.join(id.to_string()).join("journal.jsonl");
            match fs::metadata(&journal) {
                Ok(metadata) if metadata.is_file() => return Ok(id),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(util::io_err(&journal, source)),
            }
        }

        let sessions = self.read_workspace(workspace_dir, workspace)?;
        let exact: Vec<_> = sessions
            .iter()
            .filter(|session| session.info.name.as_deref() == Some(arg))
            .collect();
        if !exact.is_empty() {
            return unique_match(arg, exact);
        }

        if (4..=36).contains(&arg.len())
            && arg
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte) || byte == b'-')
        {
            let prefixed: Vec<_> = sessions
                .iter()
                .filter(|session| session.id_text.starts_with(arg))
                .collect();
            if !prefixed.is_empty() {
                return unique_match(arg, prefixed);
            }
        }

        Err(StoreError::NoMatch {
            arg: arg.into(),
            workspace: workspace.as_path().display().to_string().into(),
        })
    }

    /// Selects the newest non-archived session, with descending id as the tie-breaker.
    ///
    /// # Errors
    /// Returns a filesystem error when the workspace cannot be read.
    pub(crate) fn newest(
        &self,
        workspace_dir: &Path,
        workspace: &Workspace,
    ) -> Result<Option<SessionId>, StoreError> {
        for candidate in Self::read_candidates(workspace_dir)? {
            let session = self.read_candidate(candidate, workspace)?;
            if session.info.archived == Some(false) {
                return Ok(Some(session.info.id));
            }
        }
        Ok(None)
    }

    /// Reads cached facts or scans the journal without modifying either file.
    ///
    /// `journal_bytes` and `journal_mtime` must come from the current journal metadata.
    ///
    /// # Errors
    /// Returns a path-bearing I/O error when the journal or cache cannot be inspected.
    pub(crate) fn read_info(
        &self,
        session_dir: &Path,
        id: SessionId,
        workspace: &Workspace,
        journal_bytes: u64,
        journal_mtime: SystemTime,
    ) -> Result<SessionInfo, StoreError> {
        let journal_path = session_dir.join("journal.jsonl");
        if let Some(facts) = self.cached(&journal_path, journal_bytes, journal_mtime)
            && facts.id == id
            && facts.workspace == *workspace
        {
            return Ok(to_session_info(facts, journal_mtime)?);
        }

        let facts = match read_cache(&session_dir.join("info.json"), id, workspace, journal_bytes) {
            Some(info) => Facts {
                id,
                workspace: workspace.clone(),
                name: info.name,
                preview: info.preview,
                created_at: Some(info.created_at),
                archived: Some(info.archived),
                // The durable cache has no update sequence; only the active session knows it.
                last_seq: None,
            },
            None => scan_journal(&journal_path, id, workspace)?,
        };
        self.remember(&journal_path, journal_bytes, journal_mtime, facts.clone());
        to_session_info(facts, journal_mtime)
    }

    /// Writes the exact durable info-cache shape and updates this instance's cache.
    ///
    /// Only a session lock holder should call this after a successful journal mutation.
    ///
    /// # Errors
    /// Returns `StoreError::Invalid` for unknown cache metadata or a changed journal size, and
    /// an I/O error if the journal cannot be statted or `info.json` cannot be published.
    pub(crate) fn write_info(
        &self,
        session_dir: &Path,
        info: &SessionInfo,
        journal_bytes: u64,
    ) -> Result<(), StoreError> {
        let journal_path = session_dir.join("journal.jsonl");
        let metadata =
            fs::metadata(&journal_path).map_err(|source| util::io_err(&journal_path, source))?;
        if metadata.len() != journal_bytes {
            return Err(StoreError::Invalid {
                reason: "journal size changed before writing its info cache".into(),
            });
        }
        let journal_mtime = modified(&journal_path, &metadata)?;
        let created_at = info.created_at.ok_or_else(|| StoreError::Invalid {
            reason: "cannot cache a session without a creation time".into(),
        })?;
        let archived = info.archived.ok_or_else(|| StoreError::Invalid {
            reason: "cannot cache a session with unknown archive state".into(),
        })?;
        let cache = InfoFile {
            v: CACHE_VERSION,
            id: info.id,
            workspace: info.workspace.as_path().display().to_string().into(),
            name: info.name.clone(),
            preview: info.preview.clone(),
            created_at,
            archived,
            journal_bytes,
        };
        let bytes = sonic_rs::to_string(&cache)
            .map_err(|error| StoreError::Invalid {
                reason: format!("could not encode session info cache: {error}").into(),
            })?
            .into_bytes();
        let info_path = session_dir.join("info.json");
        util::write_atomic(&info_path, &bytes, FileMode::Mode0600)?;
        let facts = Facts {
            id: info.id,
            workspace: info.workspace.clone(),
            name: info.name.clone(),
            preview: info.preview.clone(),
            created_at: Some(created_at),
            archived: Some(archived),
            last_seq: info.last_seq,
        };
        self.remember(&journal_path, journal_bytes, journal_mtime, facts);
        Ok(())
    }

    /// Normalizes a candidate session name and checks uniqueness against workspace journals.
    ///
    /// `except` is the session being renamed, if any.
    ///
    /// # Errors
    /// Returns the existing name-validation, duplicate-name, or filesystem error.
    pub(crate) fn normalize_name(
        &self,
        workspace_dir: &Path,
        workspace: &Workspace,
        name: Option<&str>,
        except: Option<SessionId>,
    ) -> Result<Option<Box<str>>, StoreError> {
        let Some(name) = name else {
            return Ok(None);
        };
        let normalized = util::normalize_name(name)?;
        for candidate in Self::read_candidates(workspace_dir)? {
            let session = self.read_candidate(candidate, workspace)?;
            if Some(session.info.id) != except
                && session.info.name.as_deref() == Some(normalized.as_ref())
            {
                return Err(StoreError::NameTaken {
                    name: normalized,
                    id: session.info.id.to_string().into(),
                });
            }
        }
        Ok(Some(normalized))
    }

    fn read_workspace(
        &self,
        workspace_dir: &Path,
        workspace: &Workspace,
    ) -> Result<Vec<Listed>, StoreError> {
        Self::read_candidates(workspace_dir)?
            .into_iter()
            .map(|candidate| self.read_candidate(candidate, workspace))
            .collect()
    }

    fn read_candidate(
        &self,
        candidate: Candidate,
        workspace: &Workspace,
    ) -> Result<Listed, StoreError> {
        let info = self.read_info(
            &candidate.directory,
            candidate.id,
            workspace,
            candidate.journal_bytes,
            candidate.journal_mtime,
        )?;
        Ok(Listed {
            info,
            mtime_ms: candidate.mtime_ms,
            id_text: candidate.id_text,
        })
    }

    fn read_candidates(workspace_dir: &Path) -> Result<Vec<Candidate>, StoreError> {
        let directory = match fs::read_dir(workspace_dir) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(util::io_err(workspace_dir, source)),
        };
        let mut candidates = Vec::new();
        for entry in directory {
            let entry = entry.map_err(|source| util::io_err(workspace_dir, source))?;
            let session_dir = entry.path();
            if !entry
                .file_type()
                .map_err(|source| util::io_err(&session_dir, source))?
                .is_dir()
            {
                continue;
            }
            let Some(id_text) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(id) = SessionId::parse(&id_text) else {
                continue;
            };
            let journal_path = session_dir.join("journal.jsonl");
            let metadata = match fs::metadata(&journal_path) {
                Ok(metadata) if metadata.is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => return Err(util::io_err(&journal_path, source)),
            };
            let journal_mtime = modified(&journal_path, &metadata)?;
            candidates.push(Candidate {
                id,
                directory: session_dir,
                journal_bytes: metadata.len(),
                journal_mtime,
                mtime_ms: timestamp_from_mtime(journal_mtime)?.as_millisecond(),
                id_text: id_text.into_boxed_str(),
            });
        }
        candidates.sort_by(|left, right| {
            (right.mtime_ms, right.id_text.as_ref()).cmp(&(left.mtime_ms, left.id_text.as_ref()))
        });
        Ok(candidates)
    }

    fn cached(&self, path: &Path, size: u64, mtime: SystemTime) -> Option<Facts> {
        let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .get(path)
            .filter(|cached| cached.journal_bytes == size && cached.journal_mtime == mtime)
            .map(|cached| cached.facts.clone())
    }

    fn remember(&self, path: &Path, size: u64, mtime: SystemTime, facts: Facts) {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                path.to_path_buf(),
                CachedInfo {
                    journal_bytes: size,
                    journal_mtime: mtime,
                    facts,
                },
            );
    }
}

fn to_session_info(facts: Facts, mtime: SystemTime) -> Result<SessionInfo, StoreError> {
    Ok(SessionInfo {
        id: facts.id,
        name: facts.name,
        preview: facts.preview,
        workspace: facts.workspace,
        updated_at: timestamp_from_mtime(mtime)?,
        created_at: facts.created_at,
        archived: facts.archived,
        last_seq: facts.last_seq,
    })
}
fn modified(path: &Path, metadata: &fs::Metadata) -> Result<SystemTime, StoreError> {
    metadata
        .modified()
        .map_err(|source| util::io_err(path, source))
}

fn timestamp_from_mtime(mtime: SystemTime) -> Result<jiff::Timestamp, StoreError> {
    jiff::Timestamp::try_from(mtime).map_err(|error| StoreError::Invalid {
        reason: format!("journal mtime is outside the timestamp range: {error}").into(),
    })
}

fn read_cache(
    path: &Path,
    id: SessionId,
    workspace: &Workspace,
    journal_bytes: u64,
) -> Option<InfoFile> {
    let bytes = fs::read(path).ok()?;
    let info: InfoFile = sonic_rs::from_slice(&bytes).ok()?;
    let expected_workspace = workspace.as_path().display().to_string();
    (info.v == CACHE_VERSION
        && info.id == id
        && info.workspace.as_ref() == expected_workspace.as_str()
        && info.journal_bytes == journal_bytes)
        .then_some(info)
}

fn scan_journal(path: &Path, id: SessionId, workspace: &Workspace) -> Result<Facts, StoreError> {
    let mut facts = None;
    let mut first_user_seen = false;
    let mut header_matches = false;
    let scan = crate::journal::scan_prefix(path, |_, record| match record {
        Record::Session(header) => {
            if header.id != id || header.workspace != *workspace {
                return;
            }
            header_matches = true;
            facts = Some(Facts {
                id,
                workspace: header.workspace,
                name: None,
                preview: String::new().into_boxed_str(),
                created_at: Some(header.at),
                archived: Some(false),
                last_seq: None,
            });
        }
        Record::Name { name, .. } => {
            if let Some(facts) = &mut facts {
                facts.name = name;
            }
        }
        Record::Archive { archived, .. } => {
            if let Some(facts) = &mut facts {
                facts.archived = Some(archived);
            }
        }
        Record::User(Entry {
            kind: EntryKind::User { parts },
            ..
        }) if !first_user_seen => {
            first_user_seen = true;
            if let Some(facts) = &mut facts {
                facts.preview = user_preview(path, &parts).into_boxed_str();
            }
        }
        _ => {}
    });
    if scan.is_err() || !header_matches {
        return Ok(damaged_facts(id, workspace));
    }
    let Some(facts) = facts else {
        return Ok(damaged_facts(id, workspace));
    };
    Ok(facts)
}

fn user_preview(path: &Path, parts: &[JournalPart]) -> String {
    let mut preview = String::new();
    for part in parts {
        if preview.len() >= MAX_PREVIEW_BYTES {
            break;
        }
        match part {
            JournalPart::Text { text } => append_preview(&mut preview, text.as_bytes()),
            JournalPart::TextBlob { blob: digest, .. } => {
                let Ok(blob_id) = BlobId::parse(digest) else {
                    break;
                };
                let Some(session_dir) = path.parent() else {
                    break;
                };
                let blob_path = session_dir.join("blobs").join(blob_id.to_string());
                let Ok(file) = File::open(blob_path) else {
                    break;
                };
                let remaining = MAX_PREVIEW_BYTES.saturating_sub(preview.len());
                let limit = u64::try_from(remaining).unwrap_or(u64::MAX);
                let mut bytes = Vec::with_capacity(remaining);
                if file.take(limit).read_to_end(&mut bytes).is_err() {
                    append_preview(&mut preview, &bytes);
                    break;
                }
                append_preview(&mut preview, &bytes);
            }
            JournalPart::Image { .. }
            | JournalPart::ImageBlob { .. }
            | JournalPart::Blob { .. } => {}
        }
    }
    preview
}

fn append_preview(preview: &mut String, bytes: &[u8]) {
    let remaining = MAX_PREVIEW_BYTES.saturating_sub(preview.len());
    let bytes = &bytes[..bytes.len().min(remaining)];
    let valid_len = std::str::from_utf8(bytes).map_or_else(|error| error.valid_up_to(), str::len);
    if let Ok(text) = std::str::from_utf8(&bytes[..valid_len]) {
        preview.push_str(text);
    }
}

fn damaged_facts(id: SessionId, workspace: &Workspace) -> Facts {
    Facts {
        id,
        workspace: workspace.clone(),
        name: None,
        preview: DAMAGED_PREVIEW.into(),
        created_at: None,
        archived: None,
        last_seq: None,
    }
}

fn parse_cursor(value: &str) -> Result<(i64, String), StoreError> {
    let Some((millis_text, id_text)) = value.split_once(':') else {
        return Err(StoreError::MalformedCursor);
    };
    if id_text.contains(':') {
        return Err(StoreError::MalformedCursor);
    }
    let millis = millis_text
        .parse::<i64>()
        .map_err(|_| StoreError::MalformedCursor)?;
    if millis.to_string() != millis_text {
        return Err(StoreError::MalformedCursor);
    }

    if jiff::Timestamp::from_millisecond(millis).is_err() {
        return Err(StoreError::MalformedCursor);
    }
    let id = SessionId::parse(id_text).map_err(|_| StoreError::MalformedCursor)?;
    Ok((millis, id.to_string()))
}

fn unique_match(arg: &str, matches: Vec<&Listed>) -> Result<SessionId, StoreError> {
    if matches.len() == 1 {
        return Ok(matches[0].info.id);
    }
    let listed = matches
        .iter()
        .take(5)
        .map(|session| {
            let label = session
                .info
                .name
                .as_deref()
                .unwrap_or(session.info.preview.as_ref());
            let updated = session
                .info
                .updated_at
                .strftime("%Y-%m-%d %H:%M")
                .to_string();
            format!("{} ({label}, updated {updated})", session.info.id)
        })
        .collect::<Vec<_>>()
        .join(", ");
    Err(StoreError::Ambiguous {
        arg: arg.into(),
        count: matches.len(),
        listed: listed.into(),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File, FileTimes},
        io::Write,
        num::NonZeroU64,
        path::{Path, PathBuf},
        time::{Duration, UNIX_EPOCH},
    };

    use dal_core::{
        BlobId, Entry, EntryId, EntryKind, Gen, Header, JournalPart, ListQuery, Product, Record,
        SessionId, Workspace, encode,
    };

    use crate::error::StoreError;

    use super::{InfoFile, Listing, MAX_PREVIEW_BYTES};

    fn workspace(path: &Path) -> Workspace {
        Workspace::new(path.to_path_buf()).expect("absolute test workspace")
    }

    fn session_id(value: &str) -> SessionId {
        SessionId::parse(value).expect("canonical session id")
    }

    fn session_dir(root: &Path, workspace_key: &str, id: SessionId) -> PathBuf {
        root.join(workspace_key).join(id.to_string())
    }

    fn write_journal(
        dir: &Path,
        id: SessionId,
        workspace: &Workspace,
        name: Option<&str>,
        archived: bool,
        user_text: Option<&str>,
    ) -> PathBuf {
        fs::create_dir_all(dir).expect("create session directory");
        let mut records = vec![
            Record::Session(Header {
                id,
                at: jiff::Timestamp::UNIX_EPOCH,
                workspace: workspace.clone(),
                product: Product::Dalgona,
                from: None,
            }),
            Record::Boot {
                at: jiff::Timestamp::UNIX_EPOCH,
                r#gen: Gen::new(NonZeroU64::MIN),
                version: "test".into(),
            },
        ];
        if let Some(name) = name {
            records.push(Record::Name {
                at: jiff::Timestamp::UNIX_EPOCH,
                name: Some(name.into()),
            });
        }
        records.push(Record::Archive {
            at: jiff::Timestamp::UNIX_EPOCH,
            archived,
        });
        if let Some(text) = user_text {
            records.push(Record::User(Entry {
                id: EntryId::new(NonZeroU64::MIN),
                parent: None,
                at: jiff::Timestamp::UNIX_EPOCH,
                kind: EntryKind::User {
                    parts: vec![JournalPart::Text { text: text.into() }],
                },
            }));
        }
        let journal = dir.join("journal.jsonl");
        let mut file = File::create(&journal).expect("create journal");
        for record in records {
            file.write_all(&encode(&record).expect("encode record"))
                .expect("write record");
        }
        journal
    }

    fn query(limit: u32, cursor: Option<&str>, search: Option<&str>) -> ListQuery {
        ListQuery {
            limit: Some(limit),
            cursor: cursor.map(Into::into),
            search: search.map(Into::into),
        }
    }

    fn set_mtime(path: &Path, millis: u64) {
        let file = File::options()
            .write(true)
            .open(path)
            .expect("open journal to set mtime");
        file.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(millis)))
            .expect("set journal mtime");
    }

    #[test]
    fn stale_info_size_rescans_changed_journal() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let workspace_dir = root.path().join("store").join("sessions").join("work");
        let id = session_id("0192aa00-0000-7000-8000-000000000001");
        let dir = workspace_dir.join(id.to_string());
        let journal = write_journal(
            &dir,
            id,
            &workspace,
            Some("current"),
            false,
            Some("FIX the parser"),
        );
        let size = fs::metadata(&journal).expect("stat journal").len();
        let cache = InfoFile {
            v: 1,
            id,
            workspace: workspace.as_path().display().to_string().into(),
            name: Some("stale".into()),
            preview: "stale preview".into(),
            created_at: jiff::Timestamp::UNIX_EPOCH,
            archived: false,
            journal_bytes: size.saturating_sub(1),
        };
        let encoded_cache = sonic_rs::to_string(&cache).expect("encode cache");
        assert!(encoded_cache.contains("\"created_at\":\"1970-01-01T00:00:00.000Z\""));
        fs::write(dir.join("info.json"), encoded_cache).expect("write stale cache");

        let listing = Listing::new();
        let page = listing
            .list(&workspace_dir, &workspace, &query(10, None, Some("FIX")))
            .expect("list changed journal");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].name.as_deref(), Some("current"));
        assert_eq!(page.items[0].preview.as_ref(), "FIX the parser");
        assert_eq!(page.items[0].created_at, Some(jiff::Timestamp::UNIX_EPOCH));
        assert_eq!(page.items[0].archived, Some(false));
        assert_eq!(page.items[0].last_seq, None);
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .expect("open journal for rename");
        file.write_all(
            &encode(&Record::Name {
                at: jiff::Timestamp::UNIX_EPOCH,
                name: Some("renamed".into()),
            })
            .expect("encode rename"),
        )
        .expect("append rename");
        drop(file);
        let refreshed = listing
            .list(&workspace_dir, &workspace, &query(10, None, None))
            .expect("rescan resized journal");
        assert_eq!(refreshed.items[0].name.as_deref(), Some("renamed"));
    }

    #[test]
    fn text_blob_preview_reads_a_bounded_prefix() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let workspace_dir = root.path().join("store").join("work");
        let id = session_id("0192aa00-0000-7000-8000-000000000001");
        let dir = workspace_dir.join(id.to_string());
        let blobs = dir.join("blobs");
        fs::create_dir_all(&blobs).expect("create blobs directory");
        let text = "fix the parser ".repeat(400);
        let blob = BlobId::from_bytes(text.as_bytes());
        fs::write(blobs.join(blob.to_string()), &text).expect("write text blob");
        let records = [
            Record::Session(Header {
                id,
                at: jiff::Timestamp::UNIX_EPOCH,
                workspace: workspace.clone(),
                product: Product::Dalgona,
                from: None,
            }),
            Record::Boot {
                at: jiff::Timestamp::UNIX_EPOCH,
                r#gen: Gen::new(NonZeroU64::MIN),
                version: "test".into(),
            },
            Record::User(Entry {
                id: EntryId::new(NonZeroU64::MIN),
                parent: None,
                at: jiff::Timestamp::UNIX_EPOCH,
                kind: EntryKind::User {
                    parts: vec![JournalPart::TextBlob {
                        blob: blob.to_string().into(),
                        bytes: u64::try_from(text.len()).expect("text length fits u64"),
                    }],
                },
            }),
        ];
        let journal = dir.join("journal.jsonl");
        let mut file = File::create(&journal).expect("create journal");
        for record in records {
            file.write_all(&encode(&record).expect("encode record"))
                .expect("write record");
        }
        drop(file);

        let page = Listing::new()
            .list(&workspace_dir, &workspace, &query(10, None, None))
            .expect("list text-blob session");
        assert_eq!(
            page.items[0].preview.as_bytes(),
            &text.as_bytes()[..MAX_PREVIEW_BYTES]
        );
    }

    #[test]
    fn broken_header_is_listed_without_repair_or_cache_write() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let workspace_dir = root.path().join("work");
        let id = session_id("0192aa00-0000-7000-8000-000000000001");
        let dir = session_dir(&workspace_dir, "", id);
        fs::create_dir_all(&dir).expect("create session directory");
        let journal = dir.join("journal.jsonl");
        fs::write(&journal, b"not a journal\n").expect("write broken header");
        set_mtime(&journal, 1_800_000_000_123);
        let before = fs::read(&journal).expect("read original journal");
        let before_mtime = fs::metadata(&journal)
            .expect("stat original journal")
            .modified()
            .expect("original mtime");

        let empty = workspace_dir.join("0192aa00-0000-7000-8000-000000000002");
        fs::create_dir_all(empty).expect("create directory without a journal");

        let page = Listing::new()
            .list(&workspace_dir, &workspace, &query(10, None, None))
            .expect("list damaged session");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].preview.as_ref(), "(damaged session file)");
        assert_eq!(fs::read(&journal).expect("read journal after list"), before);
        assert_eq!(
            fs::metadata(&journal)
                .expect("stat journal after list")
                .modified()
                .expect("mtime after list"),
            before_mtime
        );
        assert!(!dir.join("info.json").exists());
    }

    #[test]
    fn torn_tail_listing_keeps_validated_prefix_without_repair() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let workspace_dir = root.path().join("store").join("work");
        let id = session_id("0192aa00-0000-7000-8000-000000000001");
        let dir = workspace_dir.join(id.to_string());
        let journal = write_journal(
            &dir,
            id,
            &workspace,
            Some("prefix name"),
            true,
            Some("prefix message"),
        );
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&journal)
            .expect("open journal for torn tail");
        file.write_all(b"unfinished record")
            .expect("append torn tail");
        drop(file);
        let before = fs::read(&journal).expect("read journal before listing");
        let before_mtime = fs::metadata(&journal)
            .expect("stat journal before listing")
            .modified()
            .expect("read mtime before listing");

        let page = Listing::new()
            .list(&workspace_dir, &workspace, &query(10, None, None))
            .expect("list torn-tail session");
        let session = &page.items[0];
        assert_eq!(session.name.as_deref(), Some("prefix name"));
        assert_eq!(session.preview.as_ref(), "prefix message");
        assert_eq!(session.created_at, Some(jiff::Timestamp::UNIX_EPOCH));
        assert_eq!(session.archived, Some(true));
        assert_eq!(session.last_seq, None);
        assert_eq!(
            fs::read(&journal).expect("read journal after listing"),
            before
        );
        assert_eq!(
            fs::metadata(&journal)
                .expect("stat journal after listing")
                .modified()
                .expect("read mtime after listing"),
            before_mtime
        );
        assert!(!dir.join("info.json").exists());
    }

    #[test]
    fn workspace_directory_isolation() {
        let root = tempfile::tempdir().expect("tempdir");
        let first_workspace = workspace(&root.path().join("first"));
        let second_workspace = workspace(&root.path().join("second"));
        let id = session_id("0192aa00-0000-7000-8000-000000000001");
        let first_dir = root.path().join("store").join("first");
        let second_dir = root.path().join("store").join("second");
        write_journal(
            &first_dir.join(id.to_string()),
            id,
            &first_workspace,
            Some("one"),
            false,
            Some("first"),
        );
        write_journal(
            &second_dir.join(id.to_string()),
            id,
            &second_workspace,
            Some("two"),
            false,
            Some("second"),
        );
        let listing = Listing::new();
        let first = listing
            .list(&first_dir, &first_workspace, &query(10, None, None))
            .expect("list first workspace");
        let second = listing
            .list(&second_dir, &second_workspace, &query(10, None, None))
            .expect("list second workspace");
        assert_eq!(first.items[0].name.as_deref(), Some("one"));
        assert_eq!(second.items[0].name.as_deref(), Some("two"));
    }

    #[test]
    fn mtime_tie_pagination_uses_exclusive_id_cursor() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let workspace_dir = root.path().join("store").join("work");
        let ids = [
            session_id("0192aa00-0000-7000-8000-000000000001"),
            session_id("0192aa00-0000-7000-8000-000000000002"),
            session_id("0192aa00-0000-7000-8000-000000000003"),
        ];
        for id in ids {
            let journal = write_journal(
                &workspace_dir.join(id.to_string()),
                id,
                &workspace,
                None,
                false,
                Some("entry"),
            );
            set_mtime(&journal, 1_800_000_000_123);
        }
        let listing = Listing::new();
        let first = listing
            .list(&workspace_dir, &workspace, &query(1, None, None))
            .expect("first page");
        assert_eq!(first.items[0].id, ids[2]);
        let second = listing
            .list(
                &workspace_dir,
                &workspace,
                &query(1, first.next_before.as_deref(), None),
            )
            .expect("second page");
        assert_eq!(second.items[0].id, ids[1]);
        let third = listing
            .list(
                &workspace_dir,
                &workspace,
                &query(1, second.next_before.as_deref(), None),
            )
            .expect("third page");
        assert_eq!(third.items[0].id, ids[0]);
        assert_eq!(third.next_before, None);
    }

    #[test]
    fn resolve_prefix_ambiguity_and_uppercase_rejection() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let workspace_dir = root.path().join("store").join("work");
        let ids = [
            session_id("0192aa00-0000-7000-8000-000000000001"),
            session_id("0192aa00-0000-7000-8000-000000000002"),
        ];
        for id in ids {
            write_journal(
                &workspace_dir.join(id.to_string()),
                id,
                &workspace,
                None,
                false,
                Some("text"),
            );
        }
        let listing = Listing::new();
        assert!(matches!(
            listing.resolve(&workspace_dir, &workspace, "0192aa"),
            Err(StoreError::Ambiguous { count: 2, .. })
        ));
        assert!(matches!(
            listing.resolve(&workspace_dir, &workspace, "0192AA"),
            Err(StoreError::NoMatch { .. })
        ));
        assert_eq!(
            listing
                .resolve(&workspace_dir, &workspace, &ids[0].to_string())
                .expect("full id resolves"),
            ids[0]
        );
    }

    #[test]
    fn archived_names_resolve_but_newest_skips_archived() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let workspace_dir = root.path().join("store").join("work");
        let older = session_id("0192aa00-0000-7000-8000-000000000001");
        let archived = session_id("0192aa00-0000-7000-8000-000000000002");
        let older_path = write_journal(
            &workspace_dir.join(older.to_string()),
            older,
            &workspace,
            Some("old session"),
            false,
            Some("old"),
        );
        let archived_path = write_journal(
            &workspace_dir.join(archived.to_string()),
            archived,
            &workspace,
            Some("archived session"),
            true,
            Some("new"),
        );
        set_mtime(&older_path, 1_800_000_000_100);
        set_mtime(&archived_path, 1_800_000_000_200);

        let listing = Listing::new();
        assert_eq!(
            listing
                .resolve(&workspace_dir, &workspace, "archived session")
                .expect("archived name resolves"),
            archived
        );
        assert_eq!(
            listing
                .newest(&workspace_dir, &workspace)
                .expect("find newest active"),
            Some(older)
        );
    }

    #[test]
    fn name_normalization_and_workspace_uniqueness_include_archived_sessions() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let workspace_dir = root.path().join("store").join("work");
        let id = session_id("0192aa00-0000-7000-8000-000000000001");
        write_journal(
            &workspace_dir.join(id.to_string()),
            id,
            &workspace,
            Some("parser fix"),
            true,
            Some("text"),
        );

        let listing = Listing::new();
        assert_eq!(
            listing
                .normalize_name(&workspace_dir, &workspace, Some("  alpha  "), None)
                .expect("normalize name")
                .as_deref(),
            Some("alpha")
        );
        assert!(matches!(
            listing.normalize_name(&workspace_dir, &workspace, Some("parser fix"), None),
            Err(StoreError::NameTaken { .. })
        ));
        assert!(matches!(
            listing.normalize_name(&workspace_dir, &workspace, Some("cafe"), None),
            Err(StoreError::InvalidName)
        ));
        assert_eq!(
            listing
                .normalize_name(&workspace_dir, &workspace, None, None)
                .expect("remove name"),
            None
        );
    }

    #[test]
    fn unreadable_header_has_no_fabricated_metadata() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let workspace_dir = root.path().join("store").join("work");
        let id = session_id("0192aa00-0000-7000-8000-000000000001");
        let dir = workspace_dir.join(id.to_string());
        fs::create_dir_all(&dir).expect("create session directory");
        fs::write(dir.join("journal.jsonl"), b"{}\n").expect("write invalid header");

        let page = Listing::new()
            .list(&workspace_dir, &workspace, &query(10, None, None))
            .expect("list damaged session");
        let session = &page.items[0];
        assert_eq!(session.created_at, None);
        assert_eq!(session.archived, None);
        assert_eq!(session.last_seq, None);
        assert_eq!(session.preview.as_ref(), "(damaged session file)");
    }

    #[test]
    fn malformed_cursor_and_invalid_limits_are_typed_errors() {
        let root = tempfile::tempdir().expect("tempdir");
        let workspace = workspace(root.path());
        let listing = Listing::new();
        assert!(matches!(
            listing.list(root.path(), &workspace, &query(0, None, None)),
            Err(StoreError::ListLimit)
        ));
        assert!(matches!(
            listing.list(root.path(), &workspace, &query(501, None, None)),
            Err(StoreError::ListLimit)
        ));
        assert!(matches!(
            listing.list(
                root.path(),
                &workspace,
                &query(1, Some("not-a-cursor"), None)
            ),
            Err(StoreError::MalformedCursor)
        ));
    }
}
