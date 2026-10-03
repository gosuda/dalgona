//! Logical session journals: lazy creation, ephemeral storage, recovery, and lifecycle operations.
//!
//! File journals have one actor owner. A journal enters `Lazy` without creating paths and only
//! crosses into durable storage when its first user entry is appended.

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use dal_core::{
    BlobId, BranchMode, EntryId, Gen, Header, ListQuery, Page, Product, Record, SessionId,
    SessionInfo, Workspace,
};

use crate::{
    blob::{self, PendingBlob},
    error::{BlobError, OpenReport, StoreError},
    journal::{self, Faults, Journal as FileJournal, Receipt},
    layout::SessionPaths,
    lock::LockGuard,
    shard::{Lane, Shards},
    sidecar::Sidecar,
    util,
};

/// The result of a logical append. Only `Durable` certifies a synced file batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendOutcome {
    /// Records remain buffered in a lazy session; no disk bytes were written.
    Buffered,
    /// Records and blobs were committed to the in-memory ephemeral session.
    Memory,
    /// A file batch has reached the journal and passed its fsync.
    Durable(Receipt),
}

/// Workspace-scoped access to durable and ephemeral sessions.
#[derive(Clone)]
pub struct Store {
    inner: Arc<StoreInner>,
}
impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Store")
            .field("workspace_key", &self.inner.workspace_key)
            .field("product", &self.inner.product)
            .finish_non_exhaustive()
    }
}

struct StoreInner {
    data_root: PathBuf,
    workspace: Workspace,
    product: Product,
    workspace_key: String,
    listing: crate::list::Listing,
    shards: Mutex<Option<Arc<Shards>>>,
    /// First-append journal creation is a write plus syncs; admissions are
    /// bounded to shard width so a create burst cannot flood the
    /// filesystem's sync queue faster than workers can drain it.
    create_permits: tokio::sync::Semaphore,
    #[cfg(test)]
    faults: Mutex<Faults>,
}

impl Store {
    /// Creates a store without creating or modifying any filesystem path.
    #[must_use]
    pub fn new(data_root: PathBuf, workspace: Workspace, product: Product) -> Self {
        let workspace_key = util::workspace_key(workspace.as_path());
        Self {
            inner: Arc::new(StoreInner {
                data_root,
                workspace,
                product,
                workspace_key,
                listing: crate::list::Listing::new(),
                shards: Mutex::new(None),
                create_permits: tokio::sync::Semaphore::new(crate::shard::SHARD_COUNT),
                #[cfg(test)]
                faults: Mutex::new(Faults::default()),
            }),
        }
    }

    /// Creates a session whose header, boot record, records, and blobs remain buffered until its
    /// first user entry is appended.
    pub fn create_session(&self, id: SessionId) -> Journal {
        Journal::lazy(Arc::clone(&self.inner), id, None)
    }

    /// Creates an in-memory session that never writes under the store's data root.
    pub fn ephemeral_session(&self, id: SessionId) -> Journal {
        Journal::memory(Arc::clone(&self.inner), id, None)
    }
    #[cfg(test)]
    fn set_faults_for_test(&self, faults: Faults) -> Result<(), StoreError> {
        let mut current = self.inner.faults.lock().map_err(|_| {
            util::io_err(
                &self.inner.data_root,
                io::Error::other("journal fault setter mutex is poisoned"),
            )
        })?;
        *current = faults;
        Ok(())
    }

    /// Opens and repairs a file-backed session while holding its cross-process lock.
    ///
    /// # Errors
    /// Returns the typed store, journal, or decode error without changing an invalid complete
    /// record prefix.
    pub async fn open_session(&self, id: SessionId) -> Result<(Journal, OpenReport), StoreError> {
        self.open_with_faults(id, self.faults()).await
    }

    async fn open_with_faults(
        &self,
        id: SessionId,
        faults: Faults,
    ) -> Result<(Journal, OpenReport), StoreError> {
        let paths = self.paths(id);
        let journal_path = paths.journal();
        match fs::metadata(&journal_path) {
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound { path: journal_path });
            }
            Err(source) => return Err(util::io_err(&journal_path, source)),
        }
        let (lock, opened) = open_locked_journal(&paths, id, faults).await?;
        let shards = self.shards()?;
        let mut records = opened
            .records
            .into_iter()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        let header = opened.header.clone();
        let generation = opened.r#gen;
        let report = opened.report.clone();
        let mut index = AppendIndex::from_open(&records);
        let repair = opened.repair;
        let repair_delta = opened.repair_delta;
        let mut journal_bytes = opened.journal.end();
        let mut validator = opened.validator;
        let physical = opened.journal;
        let blob_dir = paths.directory().join("blobs");
        let mut lane = shards.attach(id, physical, Some(blob_dir)).await?;
        if !repair.is_empty() {
            let bytes = encode_records(&repair)?;
            let receipt = lane
                .append(bytes, Vec::new())
                .await
                .map_err(|error| write_failure(id, error))?;
            journal_bytes = receipt.offset.saturating_add(receipt.len);
            journal_bytes_add(&mut records, &mut index, repair);
        }
        validator.commit(repair_delta);

        let journal = Journal {
            inner: Arc::clone(&self.inner),
            id,
            header,
            generation,
            records,
            index,
            journal_bytes,
            paths,
            state: State::File { lane, lock },
            pending: None,
            prelocked: None,
            ephemeral: false,
            memory_blobs: HashMap::new(),
            validator: Some(validator),
        };
        journal.refresh_from_records();
        Ok((journal, report))
    }

    /// Lists sessions in this store's configured workspace.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the query or selected workspace cannot be read.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the plan specifies Store::list takes ListQuery by value"
    )]
    pub fn list(&self, query: ListQuery) -> Result<Page<SessionInfo, Box<str>>, StoreError> {
        self.inner
            .listing
            .list(&self.workspace_dir(), &self.inner.workspace, &query)
    }

    /// Resolves a name or identifier in `workspace`.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the reference is empty, missing, or ambiguous.
    pub fn resolve(&self, workspace: &Workspace, arg: &str) -> Result<SessionId, StoreError> {
        self.inner
            .listing
            .resolve(&self.workspace_dir_for(workspace), workspace, arg)
    }

    /// Returns the newest unarchived session in the configured workspace.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the workspace listing cannot be read.
    pub fn newest(&self) -> Result<Option<SessionId>, StoreError> {
        self.inner
            .listing
            .newest(&self.workspace_dir(), &self.inner.workspace)
    }

    /// Returns the journal file path for a session without touching the filesystem.
    ///
    /// Ephemeral sessions have no durable path; the caller maps those to `None`.
    #[must_use]
    pub fn session_file(&self, id: SessionId) -> PathBuf {
        self.paths(id).journal()
    }

    /// Returns the jobs directory path for a session without touching the filesystem.
    ///
    /// Ephemeral sessions have no durable path; the caller maps those to `None`.
    #[must_use]
    pub fn session_jobs_dir(&self, id: SessionId) -> PathBuf {
        self.paths(id).jobs()
    }

    /// Reads one digest from a file-backed session without acquiring its lock.
    ///
    /// # Errors
    /// Returns [`BlobError::Gone`] after session deletion, [`BlobError::NotFound`] when only the
    /// digest is absent, or [`BlobError::Io`] for another filesystem failure.
    pub fn read_blob(&self, id: SessionId, blob_id: BlobId) -> Result<Vec<u8>, BlobError> {
        let paths = self.paths(id);
        blob::read(paths.directory(), &blob_id)
    }

    /// Publishes `bytes` durably into the session's content-addressed blob
    /// directory and returns the digest.
    ///
    /// # Errors
    /// Returns [`BlobError::TooLarge`] above the cap, or [`BlobError::Io`] on a
    /// failed publish.
    pub fn write_blob(&self, id: SessionId, bytes: &[u8]) -> Result<BlobId, BlobError> {
        let dir = self.paths(id).directory().join("blobs");
        blob::put(&dir, bytes)
    }

    /// Deletes a session tree after obtaining its nonblocking session lock.
    ///
    /// # Errors
    /// Returns [`StoreError::Locked`] without changing the tree when an actor owns the lock.
    pub fn delete(&self, id: SessionId) -> Result<(), StoreError> {
        let paths = self.paths(id);
        let journal = paths.journal();
        match fs::metadata(paths.directory()) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(StoreError::NotFound { path: journal }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound { path: journal });
            }
            Err(source) => return Err(util::io_err(paths.directory(), source)),
        }
        let _lock = LockGuard::acquire(&paths.lock(), id)?;
        match fs::metadata(&journal) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(StoreError::NotFound { path: journal }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::NotFound { path: journal });
            }
            Err(source) => return Err(util::io_err(&journal, source)),
        }
        fs::remove_dir_all(paths.directory())
            .map_err(|source| util::io_err(paths.directory(), source))
    }
    /// Forks `source` at a user entry, preserving ids on the copied parent path.
    ///
    /// # Errors
    /// Returns [`StoreError::UnknownEntry`] or [`StoreError::NotUserMessage`] for an invalid
    /// anchor, and storage errors for failed blob sharing or publication.
    pub async fn fork(
        &self,
        source: &mut Journal,
        at: EntryId,
    ) -> Result<(Journal, String), StoreError> {
        source.settle_pending().await?;
        let branch = dal_core::branch(
            &source.records,
            source.index.leaf,
            BranchMode::Fork { at },
            &source.header,
        )
        .map_err(|error| match error {
            dal_core::BranchError::NoEntries => StoreError::UnknownEntry {
                id: source.id,
                entry: at,
            },
            dal_core::BranchError::UnknownEntry { entry } => StoreError::UnknownEntry {
                id: source.id,
                entry,
            },
            dal_core::BranchError::NotUserEntry { entry } => StoreError::NotUserMessage { entry },
        })?;
        let text = anchor_text(source, &branch.anchor_parts)?;
        let mut header = branch.header;
        let id = SessionId::new_v7();
        header.id = id;
        header.at = jiff::Timestamp::now();
        header.workspace = self.inner.workspace.clone();
        header.product = self.inner.product;
        let journal = self
            .branch_to_journal(source, header, branch.records)
            .await?;
        Ok((journal, text))
    }

    /// Clones the active root-to-leaf path into a new session.
    ///
    /// # Errors
    /// Returns [`StoreError::NothingToClone`] for an empty source and storage errors for failed
    /// blob sharing or destination publication.
    pub async fn clone_session(&self, source: &mut Journal) -> Result<Journal, StoreError> {
        source.settle_pending().await?;
        let branch = dal_core::branch(
            &source.records,
            source.index.leaf,
            BranchMode::Clone,
            &source.header,
        )
        .map_err(|error| match error {
            dal_core::BranchError::NoEntries => StoreError::NothingToClone { id: source.id },
            dal_core::BranchError::UnknownEntry { entry } => StoreError::UnknownEntry {
                id: source.id,
                entry,
            },
            dal_core::BranchError::NotUserEntry { entry } => StoreError::NotUserMessage { entry },
        })?;
        let mut header = branch.header;
        header.id = SessionId::new_v7();
        header.at = jiff::Timestamp::now();
        header.workspace = self.inner.workspace.clone();
        header.product = self.inner.product;
        self.branch_to_journal(source, header, branch.records).await
    }

    async fn branch_to_journal(
        &self,
        source: &Journal,
        header: Header,
        records: Vec<Record>,
    ) -> Result<Journal, StoreError> {
        let mut destination = if source.ephemeral {
            Journal::memory_with_header(Arc::clone(&self.inner), header)
        } else {
            Journal::lazy_with_header(Arc::clone(&self.inner), header)
        };
        Self::share_branch_blobs(source, &mut destination, &records)?;
        if !records.is_empty() {
            destination.append(records).await?;
        }
        Ok(destination)
    }

    fn share_branch_blobs(
        source: &Journal,
        destination: &mut Journal,
        records: &[Record],
    ) -> Result<(), StoreError> {
        let ids = records
            .iter()
            .flat_map(blob::named_blobs)
            .collect::<std::collections::HashSet<_>>();
        if ids.is_empty() {
            return Ok(());
        }

        if destination.ephemeral {
            for id in ids {
                destination.memory_blobs.insert(id, source.read_blob(id)?);
            }
            return Ok(());
        }

        util::create_private_dir_all(destination.paths.directory())
            .map_err(|source| util::io_err(destination.paths.directory(), source))?;
        let lock = LockGuard::acquire(&destination.paths.lock(), destination.id)?;
        let blob_dir = destination.paths.directory().join("blobs");
        util::create_private_dir_all(&blob_dir)
            .map_err(|source| util::io_err(&blob_dir, source))?;
        let jobs = destination.paths.jobs();
        util::create_private_dir_all(&jobs).map_err(|source| util::io_err(&jobs, source))?;
        let source_has_file_blobs = !source.ephemeral
            && source.paths.directory().join("blobs").is_dir()
            && matches!(
                &source.state,
                State::File { .. } | State::Broken { .. } | State::Closed
            );
        if source_has_file_blobs {
            blob::share(&source.paths.directory().join("blobs"), &blob_dir, ids)?;
        } else {
            for id in ids {
                let bytes = source.read_blob(id)?;
                let published = blob::put(&blob_dir, &bytes)?;
                if published != id {
                    return Err(StoreError::Invalid {
                        reason: "source blob digest changed while sharing".into(),
                    });
                }
            }
        }
        destination.prelocked = Some(lock);
        Ok(())
    }

    fn paths(&self, id: SessionId) -> SessionPaths {
        SessionPaths::new(&self.inner.data_root, &self.inner.workspace_key, id)
    }

    fn workspace_dir(&self) -> PathBuf {
        self.inner
            .data_root
            .join("sessions")
            .join(&self.inner.workspace_key)
    }

    fn workspace_dir_for(&self, workspace: &Workspace) -> PathBuf {
        self.inner
            .data_root
            .join("sessions")
            .join(util::workspace_key(workspace.as_path()))
    }

    fn shards(&self) -> Result<Arc<Shards>, StoreError> {
        let mut shared = self.inner.shards.lock().map_err(|_| {
            util::io_err(
                &self.inner.data_root,
                io::Error::other("journal shard owner mutex is poisoned"),
            )
        })?;
        if let Some(shards) = shared.as_ref() {
            return Ok(Arc::clone(shards));
        }
        let shards = Arc::new(Shards::start()?);
        *shared = Some(Arc::clone(&shards));
        Ok(shards)
    }
    #[cfg(test)]
    fn faults(&self) -> Faults {
        use std::sync::PoisonError;

        self.inner
            .faults
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[cfg(not(test))]
    #[expect(
        clippy::unused_self,
        reason = "the test build reads faults from the store; the non-test build has none"
    )]
    fn faults(&self) -> Faults {
        Faults::default()
    }
}

/// One actor-owned logical journal.
#[expect(
    clippy::struct_field_names,
    reason = "journal_bytes names the durable journal size shared with the info cache"
)]
#[must_use = "a logical journal owns its session lock until close or drop"]
pub struct Journal {
    inner: Arc<StoreInner>,
    id: SessionId,
    header: Header,
    generation: Gen,
    records: Vec<Record>,
    index: AppendIndex,
    journal_bytes: u64,
    paths: SessionPaths,
    state: State,
    pending: Option<PendingAppend>,
    prelocked: Option<LockGuard>,
    ephemeral: bool,
    memory_blobs: HashMap<BlobId, Vec<u8>>,
    validator: Option<journal::Validator>,
}
#[expect(
    clippy::missing_fields_in_debug,
    reason = "the journal Debug view summarizes actor-visible state, not storage fields"
)]
impl std::fmt::Debug for Journal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match &self.state {
            State::Lazy { .. } => "lazy",
            State::Memory => "memory",
            State::File { .. } => "file",
            State::Broken { .. } => "broken",
            State::Closed => "closed",
        };
        formatter
            .debug_struct("Journal")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .field("ephemeral", &self.ephemeral)
            .field("state", &state)
            .field("record_count", &self.records.len())
            .field("journal_bytes", &self.journal_bytes)
            .finish()
    }
}

enum State {
    Lazy {
        blobs: Vec<PendingBlob>,
    },
    Memory,
    File {
        lane: Lane,
        lock: LockGuard,
    },
    Broken {
        lane: Option<Lane>,
        lock: Option<LockGuard>,
    },
    Closed,
}

struct PendingAppend {
    records: Vec<Record>,
    index: AppendIndex,
    journal_bytes: u64,
    generation: Gen,
    validation: journal::ValidationDelta,
    refresh_info: bool,
}

#[derive(Clone, Copy, Default)]
struct AppendIndex {
    leaf: Option<EntryId>,
    turn_records_start: Option<usize>,
}

impl AppendIndex {
    fn from_open(records: &[Record]) -> Self {
        let mut index = Self::default();
        for (record_index, record) in records.iter().enumerate() {
            index.observe(record);
            match record {
                Record::TurnStart { .. } => {
                    index.turn_records_start = Some(record_index.saturating_add(1));
                }
                Record::TurnEnd { .. } => index.turn_records_start = None,
                _ => {}
            }
        }
        index
    }

    fn parent_batch(&self, base_len: usize, records: &mut [Record]) -> Self {
        let mut next = *self;
        for (record_index, record) in records.iter_mut().enumerate() {
            if let Some(entry) = entry_mut(record) {
                entry.parent = next.leaf;
            }
            next.observe(record);
            match record {
                Record::TurnStart { .. } => {
                    next.turn_records_start =
                        Some(base_len.saturating_add(record_index).saturating_add(1));
                }
                Record::TurnEnd { .. } => next.turn_records_start = None,
                _ => {}
            }
        }
        next
    }

    fn observe(&mut self, record: &Record) {
        match record {
            Record::Leaf { to, .. } => self.leaf = *to,
            _ => {
                if let Some(entry) = record.entry() {
                    self.leaf = Some(entry.id);
                }
            }
        }
    }
}

fn entry_mut(record: &mut Record) -> Option<&mut dal_core::Entry> {
    match record {
        Record::User(entry)
        | Record::Assistant(entry)
        | Record::ToolResult(entry)
        | Record::Reminder(entry)
        | Record::Model(entry)
        | Record::Thinking(entry)
        | Record::Approval(entry)
        | Record::Mode(entry)
        | Record::Compaction(entry)
        | Record::BranchSummary(entry) => Some(entry),
        _ => None,
    }
}

impl Journal {
    fn lazy(inner: Arc<StoreInner>, id: SessionId, from: Option<dal_core::Source>) -> Self {
        let header = Header {
            id,
            at: jiff::Timestamp::now(),
            workspace: inner.workspace.clone(),
            product: inner.product,
            from,
        };
        Self::lazy_with_header(inner, header)
    }

    fn lazy_with_header(inner: Arc<StoreInner>, header: Header) -> Self {
        let id = header.id;
        let generation = Gen::new(std::num::NonZeroU64::MIN);
        let records = vec![Record::Session(header.clone()), boot_record(generation)];
        let paths = SessionPaths::new(&inner.data_root, &inner.workspace_key, id);
        let index = AppendIndex::from_open(&records);
        Self {
            inner,
            id,
            header,
            generation,
            records,
            index,
            journal_bytes: 0,
            paths,
            state: State::Lazy { blobs: Vec::new() },
            pending: None,
            prelocked: None,
            ephemeral: false,
            memory_blobs: HashMap::new(),
            validator: None,
        }
    }

    fn memory(inner: Arc<StoreInner>, id: SessionId, from: Option<dal_core::Source>) -> Self {
        let mut journal = Self::lazy(inner, id, from);
        journal.state = State::Memory;
        journal.ephemeral = true;
        journal
    }

    fn memory_with_header(inner: Arc<StoreInner>, header: Header) -> Self {
        let mut journal = Self::lazy_with_header(inner, header);
        journal.state = State::Memory;
        journal.ephemeral = true;
        journal
    }

    /// Returns the session identifier.
    #[must_use]
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// Returns the generation of this session's current boot.
    #[must_use]
    pub const fn generation(&self) -> Gen {
        self.generation
    }

    /// Returns the post-recovery in-memory record prefix, ending with the
    /// boot record this open appended, whose generation matches the open
    /// report.
    #[must_use]
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Returns whether this journal is in-memory and has no filesystem representation.
    #[must_use]
    pub const fn is_ephemeral(&self) -> bool {
        self.ephemeral
    }

    /// Returns a sidecar handle while a file-backed journal is open.
    #[must_use]
    pub fn sidecar(&self) -> Option<Sidecar<'_>> {
        match &self.state {
            State::File { .. } => Some(Sidecar::new(&self.paths)),
            State::Lazy { .. } | State::Memory | State::Broken { .. } | State::Closed => None,
        }
    }

    /// Reads a blob from this logical journal, including its in-memory blob store.
    ///
    /// # Errors
    /// Returns [`BlobError::Gone`], [`BlobError::NotFound`], or [`BlobError::Io`] for file-backed
    /// sessions, and [`BlobError::NotFound`] when an ephemeral digest is absent.
    pub fn read_blob(&self, blob_id: BlobId) -> Result<Vec<u8>, BlobError> {
        if self.ephemeral {
            return self
                .memory_blobs
                .get(&blob_id)
                .cloned()
                .ok_or(BlobError::NotFound { id: blob_id });
        }
        match &self.state {
            State::Lazy { blobs } => blobs
                .iter()
                .find(|blob| blob.id() == blob_id)
                .map(|blob| blob.bytes().to_vec())
                .ok_or(BlobError::NotFound { id: blob_id }),
            State::File { .. } | State::Broken { .. } | State::Closed => {
                blob::read(self.paths.directory(), &blob_id)
            }
            State::Memory => self
                .memory_blobs
                .get(&blob_id)
                .cloned()
                .ok_or(BlobError::NotFound { id: blob_id }),
        }
    }

    /// Publishes one content-addressed blob through this journal's storage mode.
    ///
    /// # Errors
    /// Returns a store or blob error when the session cannot durably publish the bytes.
    pub fn put_blob(&mut self, bytes: Vec<u8>) -> Result<BlobId, StoreError> {
        if matches!(&self.state, State::Lazy { .. }) {
            return Err(StoreError::Invalid {
                reason: "a blob cannot be published before the first user entry".into(),
            });
        }
        let pending = PendingBlob::prepare_put(bytes)?;
        let id = pending.id();
        match &self.state {
            State::Memory => {
                let (id, bytes) = pending.into_parts();
                self.memory_blobs.entry(id).or_insert(bytes);
                Ok(id)
            }
            State::File { .. } => {
                let dir = self.paths.directory().join("blobs");
                blob::put(&dir, pending.bytes())?;
                Ok(id)
            }
            State::Broken { .. } => Err(self.broken_error()),
            State::Closed => Err(StoreError::Invalid {
                reason: "session is closed".into(),
            }),
            State::Lazy { .. } => Err(invalid_record("session entered lazy state unexpectedly")),
        }
    }

    /// Appends typed records, returning only after its storage mode has accepted the whole batch.
    ///
    /// # Errors
    /// Returns [`StoreError`] for invalid record order, encoding, blob, or journal failures.
    pub async fn append(&mut self, records: Vec<Record>) -> Result<AppendOutcome, StoreError> {
        self.append_inner(records).await
    }
    fn ensure_validator(&mut self) -> Result<(), StoreError> {
        if self.validator.is_some() {
            return Ok(());
        }
        let mut validator = journal::Validator::new();
        let delta = validator
            .prepare_batch(&self.records)
            .map_err(validation_error)?;
        validator.commit(delta);
        self.validator = Some(validator);
        Ok(())
    }

    fn commit_validation(&mut self, delta: journal::ValidationDelta) -> Result<(), StoreError> {
        let validator = self
            .validator
            .as_mut()
            .ok_or_else(|| invalid_record("session validator is unavailable"))?;
        validator.commit(delta);
        Ok(())
    }
    fn normalize_names(&self, records: &mut [Record]) -> Result<(), StoreError> {
        let workspace_dir = self
            .inner
            .data_root
            .join("sessions")
            .join(&self.inner.workspace_key);
        for record in records {
            let Record::Name { name, .. } = record else {
                continue;
            };
            *name = if self.ephemeral {
                name.as_deref().map(util::normalize_name).transpose()?
            } else {
                self.inner.listing.normalize_name(
                    &workspace_dir,
                    &self.inner.workspace,
                    name.as_deref(),
                    Some(self.id),
                )?
            };
        }
        Ok(())
    }
    async fn append_inner(
        &mut self,
        mut records: Vec<Record>,
    ) -> Result<AppendOutcome, StoreError> {
        self.settle_pending().await?;
        match &self.state {
            State::Broken { .. } => return Err(self.broken_error()),
            State::Closed => {
                return Err(StoreError::Invalid {
                    reason: "session is closed".into(),
                });
            }
            State::Lazy { .. } | State::Memory | State::File { .. } => {}
        }
        self.normalize_names(&mut records)?;
        validate_record_values(&records)?;
        let next_index = self.index.parent_batch(self.records.len(), &mut records);
        let mut pending_blobs = Vec::new();
        for record in &mut records {
            pending_blobs.extend(blob::prepare_record(record)?);
        }
        validate_turn_totals(&self.records, &records, self.index.turn_records_start)?;
        self.ensure_validator()?;
        let validation = self
            .validator
            .as_ref()
            .ok_or_else(|| invalid_record("session validator is unavailable"))?
            .prepare_batch(&records)
            .map_err(validation_error)?;
        let had_user_entry = self
            .records
            .iter()
            .any(|record| matches!(record, Record::User(_)));
        let has_user_in_batch = records
            .iter()
            .any(|record| matches!(record, Record::User(_)));
        let refresh_info = (!had_user_entry && has_user_in_batch)
            || records
                .iter()
                .any(|record| matches!(record, Record::Name { .. } | Record::Archive { .. }));
        let generation = generation_after(self.generation, &records);

        if matches!(&self.state, State::Lazy { .. }) {
            if !has_user_in_batch {
                let State::Lazy { blobs } = &mut self.state else {
                    unreachable!("lazy state was checked above");
                };
                blobs.extend(pending_blobs);
                self.records.extend(records);
                self.index = next_index;
                self.generation = generation;
                self.commit_validation(validation)?;
                return Ok(AppendOutcome::Buffered);
            }
            let mut blobs = match &mut self.state {
                State::Lazy { blobs } => std::mem::take(blobs),
                _ => unreachable!("lazy state was checked above"),
            };
            blobs.extend(pending_blobs);
            return self
                .append_first_user(
                    records,
                    next_index,
                    blobs,
                    generation,
                    validation,
                    refresh_info,
                )
                .await;
        }

        if matches!(&self.state, State::Memory) {
            for pending in pending_blobs {
                let (id, bytes) = pending.into_parts();
                self.memory_blobs.insert(id, bytes);
            }
            self.records.extend(records);
            self.index = next_index;
            self.generation = generation;
            self.commit_validation(validation)?;
            return Ok(AppendOutcome::Memory);
        }

        self.append_file(
            records,
            next_index,
            pending_blobs,
            generation,
            validation,
            refresh_info,
        )
        .await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "first-user materialization sequences lock, blobs, journal, and cache in one place"
    )]
    async fn append_first_user(
        &mut self,
        records: Vec<Record>,
        index: AppendIndex,
        blobs: Vec<PendingBlob>,
        generation: Gen,
        validation: journal::ValidationDelta,
        refresh_info: bool,
    ) -> Result<AppendOutcome, StoreError> {
        if let Err(error) = validate_record_values(&records) {
            self.state = State::Lazy { blobs };
            return Err(error);
        }
        let bytes = match encode_records(self.records.iter().chain(records.iter())) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.state = State::Lazy { blobs };
                return Err(error);
            }
        };
        let Ok(byte_len) = u64::try_from(bytes.len()) else {
            self.state = State::Lazy { blobs };
            return Err(StoreError::Invalid {
                reason: "journal batch length exceeds the byte counter".into(),
            });
        };
        let mut current_name = None;
        if !self.ephemeral {
            for record in self.records.iter().chain(records.iter()) {
                if let Record::Name { name, .. } = record {
                    current_name = name.as_deref();
                }
            }
        }
        let current_name = current_name.map(str::to_owned);
        let store = Store {
            inner: Arc::clone(&self.inner),
        };
        let mut mark = std::time::Instant::now();
        let lap = |step: &str, mark: &mut std::time::Instant| {
            let taken = mark.elapsed();
            if taken > std::time::Duration::from_millis(250) {
                eprintln!("[dal-store] first-append {step} took {taken:?}");
            }
            *mark = std::time::Instant::now();
        };
        let shards = match store.shards() {
            Ok(shards) => shards,
            Err(error) => {
                self.state = State::Lazy { blobs };
                return Err(error);
            }
        };
        lap("shards-init", &mut mark);
        let create_permit = {
            // A permit wait has no timeout: holder starvation would park
            // every later create in silence, so report long waits.
            let mut waiting = Box::pin(self.inner.create_permits.acquire());
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(30), &mut waiting).await {
                    Ok(Ok(permit)) => break permit,
                    Ok(Err(_closed)) => {
                        self.state = State::Lazy { blobs };
                        return Err(StoreError::Invalid {
                            reason: "journal create permits are closed".into(),
                        });
                    }
                    Err(_elapsed) => eprintln!(
                        "[dal-store] session {:?} create permit outstanding",
                        self.id
                    ),
                }
            }
        };
        lap("create-permit", &mut mark);
        self.state = State::Broken {
            lane: None,
            lock: None,
        };

        // First-append setup is mkdirs, a lock acquire, name and blob
        // publishes, and the journal create — every step writes and syncs.
        // Running them on the async worker monopolizes a single-threaded
        // executor under slow storage, serializing every session's first
        // append behind one task's fsyncs; the blocking pool owns all of it,
        // bounded to shard width by `create_permits` (R-perf).
        let directory = self.paths.directory().to_path_buf();
        let blob_dir = directory.join("blobs");
        let jobs_dir = self.paths.jobs();
        let lock_path = self.paths.lock();
        let journal_path = self.paths.journal();
        let workspace_dir = self
            .inner
            .data_root
            .join("sessions")
            .join(&self.inner.workspace_key);
        let workspace = self.inner.workspace.clone();
        let inner = Arc::clone(&self.inner);
        let id = self.id;
        let prelocked = self.prelocked.take();
        let creation = tokio::task::spawn_blocking({
            let journal_path = journal_path.clone();
            move || -> Result<(LockGuard, FileJournal), StoreError> {
                let lap = |step: &str, since: std::time::Instant| {
                    let taken = since.elapsed();
                    if taken > std::time::Duration::from_millis(250) {
                        eprintln!("[dal-store] first-append {step} took {taken:?}");
                    }
                    std::time::Instant::now()
                };
                let mut mark = std::time::Instant::now();
                util::create_private_dir_all(&directory)
                    .map_err(|source| util::io_err(&directory, source))?;
                mark = lap("session-dirs", mark);
                let lock = match prelocked {
                    Some(lock) => lock,
                    None => LockGuard::acquire(&lock_path, id)?,
                };
                mark = lap("lock", mark);
                util::create_private_dir_all(&blob_dir)
                    .map_err(|source| util::io_err(&blob_dir, source))?;
                util::create_private_dir_all(&jobs_dir)
                    .map_err(|source| util::io_err(&jobs_dir, source))?;
                mark = lap("sub-dirs", mark);
                if let Some(name) = current_name.as_deref() {
                    inner.listing.normalize_name(
                        &workspace_dir,
                        &workspace,
                        Some(name),
                        Some(id),
                    )?;
                }
                mark = lap("names", mark);
                // Stage and finish every blob before one shared directory
                // sync: same durability order, a fraction of the fsyncs on
                // slow shared storage.
                let mut dirs = Vec::new();
                for blob in blobs {
                    blob::finish_staged(blob::stage_prepared(&blob_dir, blob)?, &mut dirs)?;
                }
                blob::sync_dirs(&mut dirs)?;
                mark = lap("blobs", mark);
                let journal = FileJournal::create(&journal_path, &bytes, &Faults::default())?;
                lap("journal-create", mark);
                Ok((lock, journal))
            }
        });
        let mut creation = Box::pin(creation);
        let outcome = loop {
            // The join has no timeout either: a pooled blocking task that
            // never schedules parks the permit and every later create.
            match tokio::time::timeout(std::time::Duration::from_secs(30), &mut creation).await {
                Ok(outcome) => break outcome,
                Err(_elapsed) => eprintln!(
                    "[dal-store] session {:?} journal create task outstanding",
                    self.id
                ),
            }
        };
        let (lock, file_journal) = match outcome {
            Ok(Ok(pair)) => pair,
            Ok(Err(error)) => {
                if !journal_path.exists() {
                    self.state = State::Lazy { blobs: Vec::new() };
                }
                return Err(write_failure(self.id, error));
            }
            Err(join) => {
                if !journal_path.exists() {
                    self.state = State::Lazy { blobs: Vec::new() };
                }
                return Err(StoreError::Invalid {
                    reason: format!("journal create task failed to join: {join}").into(),
                });
            }
        };
        if let State::Broken { lock: slot, .. } = &mut self.state {
            *slot = Some(lock);
        }
        drop(create_permit);
        #[cfg(test)]
        let file_journal = {
            let mut file_journal = file_journal;
            file_journal.set_faults(store.faults());
            file_journal
        };
        self.pending = Some(PendingAppend {
            records,
            index,
            journal_bytes: byte_len,
            generation,
            validation,
            refresh_info,
        });
        lap("create-join", &mut mark);
        let lane = shards
            .attach(
                self.id,
                file_journal,
                Some(self.paths.directory().join("blobs")),
            )
            .await?;
        lap("attach", &mut mark);
        let lock = match &mut self.state {
            State::Broken { lane: None, lock } => lock.take(),
            _ => None,
        }
        .ok_or(StoreError::Broken { id: self.id })?;
        self.state = State::File { lane, lock };
        let receipt = Receipt {
            offset: 0,
            len: byte_len,
        };
        self.apply_pending(receipt)?;
        Ok(AppendOutcome::Durable(receipt))
    }

    async fn append_file(
        &mut self,
        records: Vec<Record>,
        index: AppendIndex,
        blobs: Vec<PendingBlob>,
        generation: Gen,
        validation: journal::ValidationDelta,
        refresh_info: bool,
    ) -> Result<AppendOutcome, StoreError> {
        let bytes = encode_records(&records)?;
        let byte_len = u64::try_from(bytes.len()).map_err(|_| StoreError::Invalid {
            reason: "journal batch length exceeds the byte counter".into(),
        })?;
        let end = self
            .journal_bytes
            .checked_add(byte_len)
            .ok_or_else(|| StoreError::Invalid {
                reason: "journal byte counter is exhausted".into(),
            })?;
        self.pending = Some(PendingAppend {
            records,
            index,
            journal_bytes: end,
            generation,
            validation,
            refresh_info,
        });
        let result = match &mut self.state {
            State::File { lane, .. } => lane.append(bytes, blobs).await,
            _ => {
                return Err(StoreError::Invalid {
                    reason: "file append has no open lane".into(),
                });
            }
        };
        match result {
            Ok(receipt) => {
                self.apply_pending(receipt)?;
                Ok(AppendOutcome::Durable(receipt))
            }
            Err(error) => {
                let error = write_failure(self.id, error);
                self.pending = None;
                if is_irreversible(&error) {
                    self.mark_broken();
                }
                Err(error)
            }
        }
    }

    async fn settle_pending(&mut self) -> Result<(), StoreError> {
        let result = match &mut self.state {
            State::File { lane, .. }
            | State::Broken {
                lane: Some(lane), ..
            } => lane.settle().await,
            State::Lazy { .. }
            | State::Memory
            | State::Broken { lane: None, .. }
            | State::Closed => None,
        };
        match result {
            Some(Ok(receipt)) => self.apply_pending(receipt),
            Some(Err(error)) => {
                let error = write_failure(self.id, error);
                self.pending = None;
                if is_irreversible(&error) {
                    self.mark_broken();
                }
                Err(error)
            }
            None => {
                self.pending = None;
                Ok(())
            }
        }
    }

    fn apply_pending(&mut self, receipt: Receipt) -> Result<(), StoreError> {
        let Some(pending) = self.pending.take() else {
            self.mark_broken();
            return Err(StoreError::Invalid {
                reason: "a journal receipt has no staged append".into(),
            });
        };
        let expected_len = pending.journal_bytes.saturating_sub(self.journal_bytes);
        if receipt.offset != self.journal_bytes || receipt.len != expected_len {
            self.mark_broken();
            return Err(StoreError::Invalid {
                reason: "journal receipt does not match its staged batch".into(),
            });
        }
        let PendingAppend {
            records,
            index,
            journal_bytes,
            generation,
            validation,
            refresh_info,
        } = pending;
        if let Err(error) = self.commit_validation(validation) {
            self.mark_broken();
            return Err(error);
        }
        self.records.extend(records);
        self.index = index;
        self.generation = generation;
        self.journal_bytes = journal_bytes;
        if refresh_info {
            self.refresh_from_records();
        }
        Ok(())
    }

    fn mark_broken(&mut self) {
        let state = std::mem::replace(&mut self.state, State::Closed);
        self.state = match state {
            State::File { lane, lock } => State::Broken {
                lane: Some(lane),
                lock: Some(lock),
            },
            other => other,
        };
    }

    fn broken_error(&self) -> StoreError {
        StoreError::Broken { id: self.id }
    }

    /// Closes the journal and releases its file worker and cross-process lock.
    ///
    /// # Errors
    /// Returns [`StoreError::Journal`] when a prior batch is unsettled or the shard has stopped.
    pub async fn close(&mut self) -> Result<(), StoreError> {
        self.close_inner().await
    }

    /// Forks at a user message, preserving the selected tree path and returning its text.
    ///
    /// # Errors
    /// Returns [`StoreError::UnknownEntry`] or [`StoreError::NotUserMessage`] for an invalid
    /// anchor, and the applicable storage error for blob sharing or destination publication.
    pub async fn fork(&mut self, at: EntryId) -> Result<(Journal, String), StoreError> {
        let store = Store {
            inner: Arc::clone(&self.inner),
        };
        store.fork(self, at).await
    }

    /// Copies the active root-to-leaf path into a new session.
    ///
    /// # Errors
    /// Returns [`StoreError::NothingToClone`] for an empty source and the applicable storage error
    /// for blob sharing or destination publication.
    pub async fn clone_session(&mut self) -> Result<Journal, StoreError> {
        let store = Store {
            inner: Arc::clone(&self.inner),
        };
        store.clone_session(self).await
    }
    /// Changes or clears this session's name.
    ///
    /// # Errors
    /// Returns the name validation, uniqueness, or durable append error.
    pub async fn set_name(&mut self, name: Option<&str>) -> Result<AppendOutcome, StoreError> {
        self.append(vec![Record::Name {
            at: jiff::Timestamp::now(),
            name: name.map(Into::into),
        }])
        .await
    }

    /// Changes the archived state of this session.
    ///
    /// # Errors
    /// Returns the durable append error.
    pub async fn set_archived(&mut self, archived: bool) -> Result<AppendOutcome, StoreError> {
        self.append(vec![Record::Archive {
            at: jiff::Timestamp::now(),
            archived,
        }])
        .await
    }

    async fn close_inner(&mut self) -> Result<(), StoreError> {
        let settled = self.settle_pending().await;
        let file_backed = matches!(&self.state, State::File { .. });
        match &mut self.state {
            State::File { lane, .. }
            | State::Broken {
                lane: Some(lane), ..
            } => lane.close().await?,
            State::Lazy { .. }
            | State::Memory
            | State::Broken { lane: None, .. }
            | State::Closed => {}
        }
        if file_backed {
            self.refresh_from_records();
        }
        self.state = State::Closed;
        self.prelocked = None;
        self.pending = None;
        settled
    }

    fn refresh_from_records(&self) {
        if !matches!(&self.state, State::File { .. } | State::Broken { .. }) {
            return;
        }
        self.inner.listing.refresh_from_records(
            &self.paths,
            self.id,
            &self.inner.workspace,
            &self.records,
            self.journal_bytes,
        );
    }
}
fn boot_record(generation: Gen) -> Record {
    Record::Boot {
        at: jiff::Timestamp::now(),
        r#gen: generation,
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

fn encode_records<'a>(
    records: impl IntoIterator<Item = &'a Record>,
) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    for record in records {
        let line = dal_core::encode(record).map_err(|error| StoreError::Invalid {
            reason: error.to_string().into(),
        })?;
        let length = u64::try_from(line.len()).map_err(|_| {
            invalid_record("encoded journal record length exceeds the byte counter")
        })?;
        if length > journal::MAX_RECORD {
            return Err(invalid_record(
                "encoded journal record exceeds the 67108864-byte limit",
            ));
        }
        bytes.extend(line);
    }
    Ok(bytes)
}

fn map_open_failure(path: &Path, failure: journal::OpenFailure) -> StoreError {
    match failure {
        journal::OpenFailure::UnknownVersion(found) => StoreError::UnknownVersion {
            path: path.to_path_buf(),
            found,
        },
        journal::OpenFailure::Journal(error) => StoreError::Journal(error),
        journal::OpenFailure::Damaged { offset, reason } => StoreError::Damaged {
            path: path.to_path_buf(),
            offset,
            reason,
        },
    }
}

fn journal_bytes_add(records: &mut Vec<Record>, index: &mut AppendIndex, additions: Vec<Record>) {
    for record in additions {
        match &record {
            Record::TurnStart { .. } => {
                index.turn_records_start = Some(records.len().saturating_add(1));
            }
            Record::TurnEnd { .. } => index.turn_records_start = None,
            _ => {}
        }
        index.observe(&record);
        records.push(record);
    }
}
fn invalid_record(reason: impl Into<Box<str>>) -> StoreError {
    StoreError::Invalid {
        reason: reason.into(),
    }
}

fn validation_error(failure: journal::OpenFailure) -> StoreError {
    match failure {
        journal::OpenFailure::UnknownVersion(found) => {
            invalid_record(format!("unsupported journal format version {found}"))
        }
        journal::OpenFailure::Journal(error) => StoreError::Journal(error),
        journal::OpenFailure::Damaged { reason, .. } => invalid_record(reason),
    }
}

fn validate_turn_totals(
    history: &[Record],
    batch: &[Record],
    active_start: Option<usize>,
) -> Result<(), StoreError> {
    let mut turn_start = active_start;
    for (end_index, record) in batch.iter().enumerate() {
        match record {
            Record::TurnStart { .. } => {
                turn_start = Some(history.len().saturating_add(end_index).saturating_add(1));
            }
            Record::TurnEnd { usage, changes, .. } => {
                let Some(start) = turn_start else {
                    continue;
                };
                let mut turn_records = Vec::new();
                for record in history.iter().skip(start.min(history.len())) {
                    let offset = u64::try_from(turn_records.len()).unwrap_or(u64::MAX);
                    turn_records.push((offset, record.clone()));
                }
                let batch_start = start.saturating_sub(history.len()).min(end_index);
                for record in batch.iter().take(end_index).skip(batch_start) {
                    let offset = u64::try_from(turn_records.len()).unwrap_or(u64::MAX);
                    turn_records.push((offset, record.clone()));
                }
                let (actual_usage, actual_changes) =
                    journal::turn_totals(&turn_records).map_err(validation_error)?;
                if actual_usage != *usage || actual_changes != *changes {
                    return Err(invalid_record(
                        "turn end usage or file-change totals do not match the turn",
                    ));
                }
                turn_start = None;
            }
            _ => {}
        }
    }
    Ok(())
}
fn anchor_text(source: &Journal, parts: &[dal_core::JournalPart]) -> Result<String, StoreError> {
    let mut text = String::new();
    for part in parts {
        match part {
            dal_core::JournalPart::Text { text: part } => text.push_str(part),
            dal_core::JournalPart::TextBlob { blob, .. } => {
                let id = BlobId::parse(blob).map_err(|error| StoreError::Invalid {
                    reason: error.to_string().into(),
                })?;
                let bytes = source.read_blob(id)?;
                let value = String::from_utf8(bytes).map_err(|_| StoreError::Invalid {
                    reason: "fork anchor text blob is not valid UTF-8".into(),
                })?;
                text.push_str(&value);
            }
            dal_core::JournalPart::Image { .. }
            | dal_core::JournalPart::ImageBlob { .. }
            | dal_core::JournalPart::Blob { .. } => {}
        }
    }
    Ok(text)
}
fn validate_record_values(records: &[Record]) -> Result<(), StoreError> {
    for record in records {
        match record {
            Record::Assistant(entry) => {
                if let dal_core::EntryKind::Assistant {
                    content,
                    stop: dal_core::AssistantStop::Cancelled,
                    ..
                } = &entry.kind
                    && content
                        .iter()
                        .any(|block| !matches!(block, dal_core::Block::Text { .. }))
                {
                    return Err(StoreError::Invalid {
                        reason: "a cancelled assistant message holds a tool call or reasoning"
                            .into(),
                    });
                }
            }
            Record::GrantGiven { scope, .. }
                if !matches!(scope.as_ref(), "session" | "saved" | "call") =>
            {
                return Err(invalid_record(
                    "grant scope must be session, saved, or call",
                ));
            }
            Record::TurnEnd {
                stop: dal_core::TurnEndStop::Aborted,
                ..
            } => {
                return Err(invalid_record(
                    "aborted turn end is only valid during recovery",
                ));
            }
            Record::Resolved { by, .. }
            | Record::AllowAlways { by, .. }
            | Record::GrantGiven { by, .. }
                if by.as_str().is_empty() || by.as_str().len() > 128 =>
            {
                return Err(invalid_record(
                    "approval attribution must be nonempty and at most 128 bytes",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}
fn generation_after(mut generation: Gen, records: &[Record]) -> Gen {
    for record in records {
        if let Record::Boot { r#gen, .. } = record {
            generation = *r#gen;
        }
    }
    generation
}

fn is_irreversible(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Blob(_)
            | StoreError::Journal(
                crate::error::JournalError::Damaged { .. }
                    | crate::error::JournalError::ShardClosed { .. }
            )
    )
}
fn write_failure(id: SessionId, error: StoreError) -> StoreError {
    match error {
        StoreError::Journal(
            error @ crate::error::JournalError::Io {
                op: "write" | "sync",
                ..
            },
        ) => StoreError::WriteFailed {
            id,
            cause: Box::new(error),
        },
        error => error,
    }
}

/// Acquires the session lock and opens the journal on the blocking pool.
///
/// The lock acquire and the scan/repair open both write and sync, so they
/// run off the executor (R-perf). A lock held by this same process is
/// retried briefly: it always signals another journal object inside the
/// process — an owner still draining a first append or a shutdown still
/// releasing handles — never a foreign process, so a bounded wait resolves
/// the contention instead of reporting `Locked` for our own ownership.
async fn open_locked_journal(
    paths: &SessionPaths,
    id: SessionId,
    faults: Faults,
) -> Result<(LockGuard, journal::Opened), StoreError> {
    const RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
    const RETRY_POLL: std::time::Duration = std::time::Duration::from_millis(25);
    let deadline = tokio::time::Instant::now() + RETRY_BUDGET;
    let mut mark = std::time::Instant::now();
    let lap = |step: &str, mark: &mut std::time::Instant| {
        let taken = mark.elapsed();
        if taken > std::time::Duration::from_millis(250) {
            eprintln!("[dal-store] open-locked-journal {step} took {taken:?}");
        }
        *mark = std::time::Instant::now();
    };
    loop {
        let journal_path = paths.journal();
        let lock_path = paths.lock();
        let faults = faults.clone();
        let attempt = tokio::task::spawn_blocking(move || {
            let lock = LockGuard::acquire(&lock_path, id)?;
            let opened = FileJournal::open(&journal_path, &faults)
                .map_err(|failure| map_open_failure(&journal_path, failure))?;
            Ok::<_, StoreError>((lock, opened))
        })
        .await;
        lap("acquire-open", &mut mark);
        match attempt {
            Ok(Ok(pair)) => return Ok(pair),
            Ok(Err(StoreError::Locked { pid, .. }))
                if lock_might_be_ours(pid) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(RETRY_POLL).await;
            }
            Ok(Err(error)) => return Err(error),
            Err(join) => {
                return Err(StoreError::Invalid {
                    reason: format!("journal open task failed to join: {join}").into(),
                });
            }
        }
    }
}

/// The owner sidecar keeps the holder pid readable even while a lock seal
/// blocks reads of the lock file itself, so only a lock held by this process
/// is worth waiting out. An absent pid is never ours: our own acquisitions
/// publish the sidecar before contention can read it.
fn lock_might_be_ours(pid: Option<u32>) -> bool {
    pid == Some(std::process::id())
}

#[cfg(test)]
mod tests;
