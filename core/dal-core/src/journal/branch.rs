use super::{BTreeMap, Entry, EntryId, EntryKind, Header, JournalPart, Record, Source};

// Pure branch projection.

/// What a fork or clone copies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BranchMode {
    /// Copy the path through the anchor user entry's parent; the fork
    /// restarts from that entry's text.
    Fork {
        /// The user entry that restarts the branch.
        at: EntryId,
    },
    /// Copy the path through the current leaf.
    Clone,
}

/// The records of a branched session, in journal order.
#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    /// A header carrying `from` filled for the destination; the caller
    /// assigns the fresh `id`, `at`, `workspace`, and `product` before
    /// writing it.
    pub header: Header,
    /// The copied entries and path-local records, in source order.
    pub records: Vec<Record>,
    /// The fork anchor's parts; empty for a clone. The caller resolves
    /// blob parts and concatenates text parts with no separator.
    pub anchor_parts: Vec<JournalPart>,
}

/// A branch projection that cannot be made.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum BranchError {
    /// There is no path to copy.
    #[error("session has no entries to clone")]
    NoEntries,
    /// The anchor is not a user entry.
    #[error("entry {entry} is not a user message; /fork starts from a user message")]
    NotUserEntry {
        /// The offending entry.
        entry: EntryId,
    },
    /// The anchor does not exist.
    #[error("session has no entry {entry}")]
    UnknownEntry {
        /// The missing entry.
        entry: EntryId,
    },
}

/// Copies a session's chosen path into branch records without I/O.
///
/// A fork copies the path from the root through the anchor user entry's
/// parent; a clone copies it through `leaf`. Entry ids survive and
/// parents re-chain along the path. `label` records naming a path entry
/// copy with it. The returned header carries `from` describing the
/// source; its other members still describe the source session, so the
/// store rewrites `id`, `at`, `workspace`, and `product` and appends the
/// `boot` record itself.
///
/// # Errors
/// Returns [`BranchError::NoEntries`] when nothing can be cloned,
/// [`BranchError::UnknownEntry`] for a missing anchor, and
/// [`BranchError::NotUserEntry`] for a non-user anchor.
pub fn branch(
    records: &[Record],
    leaf: Option<EntryId>,
    mode: BranchMode,
    source: &Header,
) -> Result<Branch, BranchError> {
    let mut index: BTreeMap<EntryId, &Entry> = BTreeMap::new();
    for record in records {
        if let Some(entry) = record.entry() {
            index.insert(entry.id, entry);
        }
    }
    let anchor = match mode {
        BranchMode::Fork { at } => at,
        BranchMode::Clone => leaf.ok_or(BranchError::NoEntries)?,
    };
    let anchor_entry = index
        .get(&anchor)
        .ok_or(BranchError::UnknownEntry { entry: anchor })?;
    let (cutoff, anchor_parts) = match mode {
        BranchMode::Fork { .. } => match &anchor_entry.kind {
            EntryKind::User { parts } => (anchor_entry.parent, parts.clone()),
            _ => {
                return Err(BranchError::NotUserEntry {
                    entry: anchor_entry.id,
                });
            }
        },
        BranchMode::Clone => (Some(anchor_entry.id), Vec::new()),
    };
    let mut path: Vec<EntryId> = Vec::new();
    let mut cursor = cutoff;
    while let Some(id) = cursor {
        path.push(id);
        cursor = index.get(&id).and_then(|entry| entry.parent);
    }
    path.reverse();
    let mut copied = Vec::new();
    for record in records {
        let on_path = match record {
            Record::Label { entry, .. } => path.contains(entry),
            _ => record.entry().is_some_and(|entry| path.contains(&entry.id)),
        };
        if on_path {
            copied.push(record.clone());
        }
    }
    let header = Header {
        id: source.id,
        at: source.at,
        workspace: source.workspace.clone(),
        product: source.product,
        from: Some(Source {
            session: source.id,
            entry: Some(anchor),
        }),
    };
    Ok(Branch {
        header,
        records: copied,
        anchor_parts,
    })
}
