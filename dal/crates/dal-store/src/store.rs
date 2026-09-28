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
    workspace_key: Box<str>,
    listing: crate::list::Listing,
    shards: Mutex<Option<Arc<Shards>>>,
    faults: Mutex<Faults>,
}

impl Store {
    /// Creates a store without creating or modifying any filesystem path.
    #[must_use]
    pub fn new(data_root: PathBuf, workspace: Workspace, product: Product) -> Self {
        let workspace_key: Box<str> = util::workspace_key(workspace.as_path()).into();
        Self {
            inner: Arc::new(StoreInner {
                data_root,
                workspace,
                product,
                workspace_key,
                listing: crate::list::Listing::new(),
                shards: Mutex::new(None),
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
        self.open_with_faults(id, self.faults()?).await
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
        let lock = LockGuard::acquire(paths.directory(), id)?;
        let shards = self.shards()?;
        let opened = FileJournal::open(&journal_path, &faults)
            .map_err(|failure| map_open_failure(&journal_path, failure))?;
        let mut records = opened
            .records
            .into_iter()
            .map(|(_, record)| record)
            .collect::<Vec<_>>();
        let header = opened.header;
        let generation = opened.r#gen;
        let report = opened.report;
        // Recovery closes any open turn before this handle is published.
        let index = AppendIndex {
            leaf: opened.leaf,
            turn_records_start: None,
        };
        let repair = opened.repair;
        let repair_delta = opened.repair_delta;
        let mut journal_bytes = opened.journal.end();
        let mut validator = opened.validator;
        let physical = opened.journal;
        let blob_dir = paths.directory().join("blobs");
        let mut lane = shards.attach(id, physical, Some(blob_dir)).await?;
        if !repair.is_empty() {
            let bytes = encode_records(&repair)?;
            let receipt = lane.append(bytes, Vec::new()).await?;
            journal_bytes = receipt.offset.saturating_add(receipt.len);
            records.extend(repair);
        }
        validator.commit(repair_delta);

        let journal = Journal {
            inner: Arc::clone(&self.inner),
            id,
            header,
            generation,
            records,
            index,
            durable_bytes: journal_bytes,
            paths,
            state: State::File { lane, lock },
            pending: None,
            prelocked: None,
            ephemeral: false,
            memory_blobs: HashMap::new(),
            validator: Some(validator),
        };
        journal.refresh_info_cache();
        Ok((journal, report))
    }

    /// Lists sessions in this store's configured workspace.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the query or selected workspace cannot be read.
    pub fn list(&self, query: ListQuery) -> Result<Page<SessionInfo, Box<str>>, StoreError> {
        self.inner
            .listing
            .list(&self.workspace_dir(), &self.inner.workspace, query)
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

    /// Reads one digest from a file-backed session without acquiring its lock.
    ///
    /// # Errors
    /// Returns [`BlobError::Gone`] after session deletion, [`BlobError::NotFound`] when only the
    /// digest is absent, or [`BlobError::Io`] for another filesystem failure.
    pub fn read_blob(&self, id: SessionId, blob_id: BlobId) -> Result<Vec<u8>, BlobError> {
        let paths = self.paths(id);
        blob::read(paths.directory(), &blob_id)
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
        let _lock = LockGuard::acquire(paths.directory(), id)?;
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
            dal_core::BranchError::NoEntries => StoreError::NothingToClone {
                id: source.id.to_string().into(),
            },
            dal_core::BranchError::UnknownEntry { entry } => StoreError::UnknownEntry {
                id: source.id.to_string().into(),
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
            dal_core::BranchError::NoEntries => StoreError::NothingToClone {
                id: source.id.to_string().into(),
            },
            dal_core::BranchError::UnknownEntry { entry } => StoreError::UnknownEntry {
                id: source.id.to_string().into(),
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

        fs::create_dir_all(destination.paths.directory())
            .map_err(|source| util::io_err(destination.paths.directory(), source))?;
        let lock = LockGuard::acquire(destination.paths.directory(), destination.id)?;
        let blob_dir = destination.paths.directory().join("blobs");
        fs::create_dir_all(&blob_dir).map_err(|source| util::io_err(&blob_dir, source))?;
        let jobs_dir = destination.paths.jobs();
        fs::create_dir_all(&jobs_dir).map_err(|source| util::io_err(&jobs_dir, source))?;
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
        self.workspace_dir_for(&self.inner.workspace)
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
    fn faults(&self) -> Result<Faults, StoreError> {
        self.inner
            .faults
            .lock()
            .map(|faults| faults.clone())
            .map_err(|_| {
                util::io_err(
                    &self.inner.data_root,
                    io::Error::other("journal fault setter mutex is poisoned"),
                )
            })
    }
}

/// One actor-owned logical journal.
#[must_use = "a logical journal owns its session lock until close or drop"]
pub struct Journal {
    inner: Arc<StoreInner>,
    id: SessionId,
    header: Header,
    generation: Gen,
    records: Vec<Record>,
    index: AppendIndex,
    durable_bytes: u64,
    paths: SessionPaths,
    state: State,
    pending: Option<PendingAppend>,
    prelocked: Option<LockGuard>,
    ephemeral: bool,
    memory_blobs: HashMap<BlobId, Vec<u8>>,
    validator: Option<journal::Validator>,
}
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
            .field("journal_bytes", &self.durable_bytes)
            .finish_non_exhaustive()
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

struct FirstUserStorage {
    blobs: Vec<PendingBlob>,
    blob_dir: PathBuf,
    lock: LockGuard,
    shards: Arc<Shards>,
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
            durable_bytes: 0,
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

    /// Borrows accepted records without exposing a pending append.
    ///
    /// Lazy and ephemeral journals include their accepted in-memory records;
    /// file-backed records become visible only after the append receipt.
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
    pub fn sidecar(&self) -> Option<Sidecar> {
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
                .ok_or_else(|| BlobError::NotFound {
                    hex: blob_id.to_string().into(),
                });
        }
        match &self.state {
            State::Lazy { blobs } => blobs
                .iter()
                .find(|blob| blob.id() == blob_id)
                .map(|blob| blob.bytes().to_vec())
                .ok_or_else(|| BlobError::NotFound {
                    hex: blob_id.to_string().into(),
                }),
            State::File { .. } | State::Broken { .. } | State::Closed => {
                blob::read(self.paths.directory(), &blob_id)
            }
            State::Memory => {
                self.memory_blobs
                    .get(&blob_id)
                    .cloned()
                    .ok_or_else(|| BlobError::NotFound {
                        hex: blob_id.to_string().into(),
                    })
            }
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
            .join(std::path::Path::new(&*self.inner.workspace_key));
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
        let materialized = self
            .records
            .iter()
            .any(|record| matches!(record, Record::User(_)));
        let has_user = records
            .iter()
            .any(|record| matches!(record, Record::User(_)));
        let refresh_info = (!materialized && has_user)
            || records
                .iter()
                .any(|record| matches!(record, Record::Name { .. } | Record::Archive { .. }));
        let generation = generation_after(self.generation, &records);

        if matches!(&self.state, State::Lazy { .. }) {
            if !has_user {
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

    async fn append_first_user(
        &mut self,
        records: Vec<Record>,
        index: AppendIndex,
        blobs: Vec<PendingBlob>,
        generation: Gen,
        validation: journal::ValidationDelta,
        refresh_info: bool,
    ) -> Result<AppendOutcome, StoreError> {
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
        let FirstUserStorage {
            blobs,
            blob_dir,
            lock,
            shards,
        } = self.prepare_first_user_storage(&records, blobs)?;
        self.state = State::Broken {
            lane: None,
            lock: Some(lock),
        };
        for blob in blobs {
            blob::put_prepared(&blob_dir, blob)?;
        }

        let store = Store {
            inner: Arc::clone(&self.inner),
        };
        let journal_path = self.paths.journal();
        let mut file_journal = match FileJournal::create(&journal_path, &bytes, &Faults::default())
        {
            Ok(journal) => journal,
            Err(error) => {
                if !journal_path.exists() {
                    let state = std::mem::replace(&mut self.state, State::Closed);
                    if let State::Broken {
                        lock: Some(lock), ..
                    } = state
                    {
                        self.prelocked = Some(lock);
                    }
                    self.state = State::Lazy { blobs: Vec::new() };
                }
                return Err(error.into());
            }
        };
        file_journal.set_faults(store.faults()?);
        self.pending = Some(PendingAppend {
            records,
            index,
            journal_bytes: byte_len,
            generation,
            validation,
            refresh_info,
        });
        let lane = shards
            .attach(
                self.id,
                file_journal,
                Some(self.paths.directory().join("blobs")),
            )
            .await?;
        let lock = match &mut self.state {
            State::Broken { lane: None, lock } => lock.take(),
            _ => None,
        }
        .ok_or_else(|| StoreError::Broken {
            id: self.id.to_string().into(),
        })?;
        self.state = State::File { lane, lock };
        let receipt = Receipt {
            offset: 0,
            len: byte_len,
        };
        self.apply_pending(receipt)?;
        Ok(AppendOutcome::Durable(receipt))
    }

    fn prepare_first_user_storage(
        &mut self,
        records: &[Record],
        blobs: Vec<PendingBlob>,
    ) -> Result<FirstUserStorage, StoreError> {
        if let Err(source) = fs::create_dir_all(self.paths.directory()) {
            self.state = State::Lazy { blobs };
            return Err(util::io_err(self.paths.directory(), source));
        }
        let lock = match self.prelocked.take() {
            Some(lock) => lock,
            None => match LockGuard::acquire(self.paths.directory(), self.id) {
                Ok(lock) => lock,
                Err(error) => {
                    self.state = State::Lazy { blobs };
                    return Err(error);
                }
            },
        };
        let blob_dir = self.paths.directory().join("blobs");
        let prepared = (|| {
            fs::create_dir_all(&blob_dir).map_err(|source| util::io_err(&blob_dir, source))?;
            let jobs_dir = self.paths.jobs();
            fs::create_dir_all(&jobs_dir).map_err(|source| util::io_err(&jobs_dir, source))?;
            self.normalize_first_user_name(records)?;
            Store {
                inner: Arc::clone(&self.inner),
            }
            .shards()
        })();
        match prepared {
            Ok(shards) => Ok(FirstUserStorage {
                blobs,
                blob_dir,
                lock,
                shards,
            }),
            Err(error) => {
                self.prelocked = Some(lock);
                self.state = State::Lazy { blobs };
                Err(error)
            }
        }
    }

    fn normalize_first_user_name(&self, records: &[Record]) -> Result<(), StoreError> {
        if self.ephemeral {
            return Ok(());
        }
        let current_name = self
            .records
            .iter()
            .chain(records)
            .rev()
            .find_map(|record| match record {
                Record::Name { name, .. } => Some(name.as_deref()),
                _ => None,
            })
            .flatten();
        let Some(name) = current_name else {
            return Ok(());
        };
        let workspace_dir = self
            .inner
            .data_root
            .join("sessions")
            .join(std::path::Path::new(&*self.inner.workspace_key));
        self.inner.listing.normalize_name(
            &workspace_dir,
            &self.inner.workspace,
            Some(name),
            Some(self.id),
        )?;
        Ok(())
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
            .durable_bytes
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
        let expected_len = pending.journal_bytes.saturating_sub(self.durable_bytes);
        if receipt.offset != self.durable_bytes || receipt.len != expected_len {
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
        self.durable_bytes = journal_bytes;
        if refresh_info {
            self.refresh_info_cache();
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
        StoreError::Broken {
            id: self.id.to_string().into(),
        }
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
            self.refresh_info_cache();
        }
        self.state = State::Closed;
        self.prelocked = None;
        self.pending = None;
        settled
    }

    fn refresh_info_cache(&self) {
        if !matches!(&self.state, State::File { .. } | State::Broken { .. }) {
            return;
        }
        let path = self.paths.journal();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(target: "dalgon.store", %error, "could not stat journal for info cache");
                return;
            }
        };
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                tracing::warn!(target: "dalgon.store", %error, "could not read journal mtime for info cache");
                return;
            }
        };
        let info = match self.inner.listing.read_info(
            self.paths.directory(),
            self.id,
            &self.inner.workspace,
            self.durable_bytes,
            modified,
        ) {
            Ok(info) => info,
            Err(error) => {
                tracing::warn!(target: "dalgon.store", %error, "could not read session info for cache update");
                return;
            }
        };
        if let Err(error) =
            self.inner
                .listing
                .write_info(self.paths.directory(), &info, self.durable_bytes)
        {
            tracing::warn!(target: "dalgon.store", %error, "could not write session info cache");
        }
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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        future::Future,
        num::NonZeroU64,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        task::{Context, Poll, Waker},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::blob::INLINE_LIMIT;
    use dal_core::{
        AssistantStop, Block, CallId, Entry, EntryId, EntryKind, Family, JournalPart, ListQuery,
        Product, RawJson, Record, SessionId, TurnEndStop, TurnId, Usage, Workspace,
    };

    use super::*;
    use crate::journal::FaultSwitch;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(1);
            let time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "dal-store-{}-{time}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).expect("create store test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn store(temp: &TempDir) -> Store {
        let workspace = Workspace::new(temp.path().join("workspace"))
            .expect("temporary workspace path is absolute");
        Store::new(temp.path().join("data"), workspace, Product::Dalgona)
    }

    fn entry_id(value: u64) -> EntryId {
        EntryId::new(NonZeroU64::new(value).unwrap_or(NonZeroU64::MIN))
    }

    fn timestamp() -> jiff::Timestamp {
        jiff::Timestamp::now()
    }

    fn user(id: u64, text: impl Into<Box<str>>) -> Record {
        Record::User(Entry {
            id: entry_id(id),
            parent: None,
            at: timestamp(),
            kind: EntryKind::User {
                parts: vec![JournalPart::Text { text: text.into() }],
            },
        })
    }

    fn usage() -> Usage {
        Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        }
    }

    fn assistant_with_calls(id: u64, calls: &[&str]) -> Record {
        let input = RawJson::parse("{}").expect("valid tool input");
        let content = calls
            .iter()
            .map(|call| Block::ToolCall {
                id: CallId::new(*call),
                name: "read_file".into(),
                input: input.clone(),
            })
            .collect();
        Record::Assistant(Entry {
            id: entry_id(id),
            parent: None,
            at: timestamp(),
            kind: EntryKind::Assistant {
                api: Family::Chat,
                model: "test-model".into(),
                content,
                usage: usage(),
                stop: AssistantStop::ToolUse,
            },
        })
    }

    #[tokio::test]
    async fn lazy_first_user_writes_one_durable_header_boot_and_user_batch() {
        let temp = TempDir::new();
        let store = store(&temp);
        let data_root = store.inner.data_root.clone();
        let id = SessionId::new_v7();
        assert!(!data_root.exists());
        let mut journal = store.create_session(id);

        assert_eq!(
            journal
                .append(vec![Record::Name {
                    at: timestamp(),
                    name: Some("parser fix".into()),
                }])
                .await
                .expect("buffer name"),
            AppendOutcome::Buffered
        );
        assert!(!data_root.exists());

        let receipt = match journal
            .append(vec![user(1, "first message")])
            .await
            .expect("durable first user")
        {
            AppendOutcome::Durable(receipt) => receipt,
            other => panic!("expected a durable receipt, got {other:?}"),
        };
        let bytes = fs::read(journal.paths.journal()).expect("read journal");
        assert_eq!(receipt.offset, 0);
        assert_eq!(
            receipt.len,
            u64::try_from(bytes.len()).expect("file length fits u64")
        );
        let records = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| dal_core::decode(line).expect("decode complete line").record)
            .collect::<Vec<_>>();
        assert_eq!(records.len(), 4);
        assert!(matches!(&records[0], Record::Session(_)));
        assert!(matches!(&records[1], Record::Boot { .. }));
        assert!(matches!(&records[2], Record::Name { .. }));
        assert!(matches!(&records[3], Record::User(_)));
        assert!(journal.paths.directory().join("info.json").is_file());
        journal.close().await.expect("close file session");
    }

    #[tokio::test]
    async fn restore_uses_the_store_boot_without_recovery_effects() {
        let temp = TempDir::new();
        let store = store(&temp);
        let id = SessionId::new_v7();
        let mut journal = store.create_session(id);
        let initial = dal_core::Session::restore(journal.records().iter().cloned())
            .expect("restore lazy boot");
        assert!(matches!(initial.phase(), dal_core::Phase::Idle));
        assert_eq!(journal.generation().get(), 1);

        let turn = TurnId::new(NonZeroU64::MIN);
        journal
            .append(vec![
                Record::TurnStart {
                    at: timestamp(),
                    turn,
                },
                user(1, "first turn"),
                Record::TurnEnd {
                    at: timestamp(),
                    turn,
                    stop: TurnEndStop::Done,
                    usage: None,
                    changes: Vec::new(),
                },
            ])
            .await
            .expect("persist completed turn");
        journal.close().await.expect("close original session");

        let (mut reopened, _) = store.open_session(id).await.expect("reopen session");
        assert_eq!(reopened.generation().get(), 2);
        let mut session = dal_core::Session::restore(reopened.records().iter().cloned())
            .expect("restore already recovered session");
        let mut effects = Vec::new();
        session
            .step(
                dal_core::Event::Command {
                    cmd: dal_core::Command::Prompt {
                        expect: dal_core::Expect::Idle,
                        content: vec![dal_core::Part::Text {
                            text: "next turn".into(),
                        }],
                    },
                    by: dal_core::ClientId::new("test"),
                },
                timestamp(),
                &mut effects,
            )
            .expect("next prompt");
        assert!(
            matches!(session.phase(), dal_core::Phase::Opening { turn, .. } if turn.get() == 2)
        );
        assert_eq!(
            reopened
                .records()
                .iter()
                .filter(|record| matches!(record, Record::Boot { .. }))
                .count(),
            2
        );
        assert!(!effects.iter().any(|effect| matches!(effect,
            dal_core::Effect::Emit(emit) if emit.records.iter().any(|record| matches!(record, Record::Boot { .. }))
        )));
        reopened.close().await.expect("close restored session");
    }

    #[tokio::test]
    async fn ephemeral_blobs_and_records_leave_no_files_before_or_after_close() {
        let temp = TempDir::new();
        let store = store(&temp);
        let data_root = store.inner.data_root.clone();
        let mut journal = store.ephemeral_session(SessionId::new_v7());
        let text = "x".repeat(INLINE_LIMIT);
        let expected = text.as_bytes().to_vec();

        assert_eq!(
            journal
                .append(vec![user(1, text)])
                .await
                .expect("append in memory"),
            AppendOutcome::Memory
        );
        assert!(journal.is_ephemeral());
        assert!(journal.sidecar().is_none());
        let user_record = journal
            .records
            .iter()
            .find(|record| matches!(record, Record::User(_)))
            .expect("user record");
        let blob_ids = blob::named_blobs(user_record);
        let [blob_id] = blob_ids.as_slice() else {
            panic!("threshold-sized user text names one blob");
        };
        assert_eq!(
            journal.read_blob(*blob_id).expect("read memory blob"),
            expected
        );
        assert!(!data_root.exists());
        assert!(
            store
                .list(ListQuery {
                    limit: None,
                    cursor: None,
                    search: None,
                })
                .expect("list empty workspace")
                .items
                .is_empty()
        );

        journal.close().await.expect("close memory session");
        assert_eq!(
            journal
                .read_blob(*blob_id)
                .expect("memory blob survives close"),
            expected
        );
        assert!(!data_root.exists());
    }

    #[tokio::test]
    async fn locked_session_cannot_be_opened_or_deleted_until_close() {
        let temp = TempDir::new();
        let store = store(&temp);
        let id = SessionId::new_v7();
        let mut journal = store.create_session(id);
        let text = "b".repeat(INLINE_LIMIT);
        let blob_id = BlobId::from_bytes(text.as_bytes());
        journal
            .append(vec![user(1, text)])
            .await
            .expect("create file session");

        assert!(matches!(
            store.open_session(id).await,
            Err(StoreError::Locked { .. })
        ));
        assert!(matches!(store.delete(id), Err(StoreError::Locked { .. })));

        journal.close().await.expect("release session lock");
        store.delete(id).expect("delete closed session");
        assert!(matches!(store.read_blob(id, blob_id), Err(BlobError::Gone)));
    }

    #[tokio::test]
    async fn recovery_aborts_calls_once_with_started_and_not_run_outcomes() {
        let temp = TempDir::new();
        let store = store(&temp);
        let id = SessionId::new_v7();
        let mut journal = store.create_session(id);
        journal
            .append(vec![user(1, "run the tools")])
            .await
            .expect("create session");
        let turn = TurnId::new(NonZeroU64::MIN);
        journal
            .append(vec![
                Record::TurnStart {
                    at: timestamp(),
                    turn,
                },
                assistant_with_calls(2, &["started", "not-started"]),
            ])
            .await
            .expect("start unfinished turn");
        journal
            .append(vec![Record::ToolStart {
                at: timestamp(),
                turn,
                call: CallId::new("started"),
            }])
            .await
            .expect("start first call");
        journal.close().await.expect("close interrupted session");

        let (mut reopened, report) = store
            .open_session(id)
            .await
            .expect("repair interrupted session");
        let aborted = report.aborted.as_ref().expect("aborted turn fact");
        assert_eq!(aborted.turn, turn);
        assert_eq!(aborted.interrupted, 1);
        assert_eq!(aborted.not_run, 1);
        let mut interrupted_text = false;
        let mut not_run_text = false;
        for record in &reopened.records {
            let Record::ToolResult(entry) = record else {
                continue;
            };
            let EntryKind::ToolResult { parts, .. } = &entry.kind else {
                continue;
            };
            for part in parts {
                if let JournalPart::Text { text } = part {
                    interrupted_text |= text.as_ref() == crate::error::INTERRUPTED_CALL;
                    not_run_text |= text.as_ref() == crate::error::NOT_RUN_CALL;
                }
            }
        }
        assert!(interrupted_text);
        assert!(not_run_text);
        assert_eq!(
            reopened
                .records
                .iter()
                .filter(|record| matches!(
                    record,
                    Record::TurnEnd {
                        stop: TurnEndStop::Aborted,
                        ..
                    }
                ))
                .count(),
            1
        );
        reopened.close().await.expect("close repaired session");

        let (mut second, second_report) = store
            .open_session(id)
            .await
            .expect("reopen repaired session");
        assert!(second_report.aborted.is_none());
        second.close().await.expect("close second open");
    }

    #[tokio::test]
    async fn fork_is_lazy_and_clone_shares_blobs_before_publishing_journal() {
        let temp = TempDir::new();
        let store = store(&temp);
        let source_id = SessionId::new_v7();
        let mut source = store.create_session(source_id);
        let text = "c".repeat(INLINE_LIMIT);
        let bytes = text.as_bytes().to_vec();
        let blob_id = BlobId::from_bytes(&bytes);
        source
            .append(vec![user(1, text.clone())])
            .await
            .expect("create source session");

        let (mut fork, restart_text) = store
            .fork(&mut source, entry_id(1))
            .await
            .expect("fork first user entry");
        assert_eq!(restart_text, text);
        assert!(matches!(&fork.state, State::Lazy { .. }));
        assert!(!fork.paths.directory().exists());

        let mut clone = store
            .clone_session(&mut source)
            .await
            .expect("clone active path");
        let copied_user = clone
            .records
            .iter()
            .find_map(|record| record.entry())
            .expect("copied tree entry");
        assert_eq!(copied_user.id, entry_id(1));
        #[cfg(unix)]
        let source_blob = source
            .paths
            .directory()
            .join("blobs")
            .join(blob_id.to_string());
        let clone_blob = clone
            .paths
            .directory()
            .join("blobs")
            .join(blob_id.to_string());
        assert_eq!(
            fs::read(&clone_blob).expect("read shared clone blob"),
            bytes
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::metadata(&source_blob)
                    .expect("source blob metadata")
                    .ino(),
                fs::metadata(&clone_blob)
                    .expect("clone blob metadata")
                    .ino()
            );
        }
        if let Record::Session(header) = &clone.records[0] {
            let from = header.from.as_ref().expect("clone provenance");
            assert_eq!(from.session, source_id);
            assert_eq!(from.entry, Some(entry_id(1)));
        } else {
            panic!("clone starts with a session header");
        }

        fork.close().await.expect("close lazy fork");
        clone.close().await.expect("close clone");
        source.close().await.expect("close source");
    }

    #[tokio::test]
    async fn cancelled_admitted_append_settles_before_the_next_append() {
        let temp = TempDir::new();
        let store = store(&temp);
        let id = SessionId::new_v7();
        let mut journal = store.create_session(id);
        journal
            .append(vec![user(1, "cancel test")])
            .await
            .expect("create file session");

        let shards = store.shards().expect("start shard workers");
        let (started, release) = shards.hold_worker_for_test(id).expect("queue worker hold");
        tokio::task::spawn_blocking(move || started.recv())
            .await
            .expect("wait task")
            .expect("worker started hold");

        let mut context = Context::from_waker(Waker::noop());
        let future = journal.append(vec![Record::Name {
            at: timestamp(),
            name: Some("settled before next".into()),
        }]);
        let mut future = Box::pin(future);
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        drop(future);
        release.send(()).expect("release held shard");

        journal
            .append(vec![Record::Archive {
                at: timestamp(),
                archived: false,
            }])
            .await
            .expect("next append settles and follows prior append");
        assert!(journal.records.iter().any(|record| matches!(
            record,
            Record::Name {
                name: Some(name),
                ..
            } if name.as_ref() == "settled before next"
        )));
        journal.close().await.expect("close after settlement");
    }

    #[tokio::test]
    async fn blob_publish_failure_keeps_record_unpublished_and_marks_broken() {
        let temp = TempDir::new();
        let store = store(&temp);
        let id = SessionId::new_v7();
        let mut journal = store.create_session(id);
        journal
            .append(vec![user(1, "existing record")])
            .await
            .expect("create file session");
        let before = fs::read(journal.paths.journal()).expect("read acknowledged bytes");
        let blob_dir = journal.paths.directory().join("blobs");
        fs::remove_dir(&blob_dir).expect("remove empty blob directory");
        fs::write(&blob_dir, b"not a directory").expect("block blob publication");

        let error = journal
            .append(vec![user(2, "d".repeat(INLINE_LIMIT))])
            .await
            .expect_err("blob publication fails");
        assert!(matches!(error, StoreError::Blob(BlobError::Io { .. })));
        assert_eq!(
            fs::read(journal.paths.journal()).expect("read journal after blob failure"),
            before
        );
        assert_eq!(journal.records.len(), 3);
        assert!(matches!(
            journal
                .append(vec![Record::Archive {
                    at: timestamp(),
                    archived: false,
                }])
                .await,
            Err(StoreError::Broken { .. })
        ));
        journal.close().await.expect("close broken journal");
    }

    #[tokio::test]
    async fn failed_rollback_marks_broken_until_reopen_repairs_tail() {
        let temp = TempDir::new();
        let store = store(&temp);
        store
            .set_faults_for_test(Faults {
                write_after: Some(1),
                truncate: FaultSwitch::ON,
                ..Faults::default()
            })
            .expect("configure append failpoint");
        let id = SessionId::new_v7();
        let mut journal = store.create_session(id);
        journal
            .append(vec![user(1, "durable prefix")])
            .await
            .expect("create durable file before enabling writer fault");

        let error = journal
            .append(vec![Record::Name {
                at: timestamp(),
                name: Some("will not commit".into()),
            }])
            .await
            .expect_err("partial write and rollback fail");
        assert!(matches!(
            error,
            StoreError::Journal(crate::error::JournalError::Damaged { .. })
        ));
        assert!(matches!(
            journal
                .append(vec![Record::Archive {
                    at: timestamp(),
                    archived: false,
                }])
                .await,
            Err(StoreError::Broken { .. })
        ));
        journal.close().await.expect("close broken writer");

        store
            .set_faults_for_test(Faults::default())
            .expect("clear append failpoint");
        let (mut reopened, report) = store.open_session(id).await.expect("reopen and repair");
        assert!(report.torn.is_some());
        assert!(reopened.records.iter().all(|record| {
            !matches!(record, Record::Name { name: Some(name), .. } if name.as_ref() == "will not commit")
        }));
        reopened.close().await.expect("close repaired writer");
    }
}
