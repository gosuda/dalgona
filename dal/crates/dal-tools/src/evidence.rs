//! The Hashline evidence owner: consumer-bound read views, recipient-bound
//! delivery, and explicit parent-cutoff adoption (R06 E06).
//!
//! A view is intact only while its provenance names a live capture bound to
//! the provenance consumer in the same session, its path and header are the
//! capture's own, and every complete row repeats the captured line exactly.
//! Anything else, including copied or restyled rows, earns no evidence.

use std::{borrow::Cow, path::Path, sync::Arc};

use dal_agent::ext::{AdoptError, Evidence};
use dal_core::{Consumer, GenerationId, Provenance, ReadView, SessionId, SourceRow};

use crate::patch::snapshot::{ReadRef, Snapshot, SnapshotStore};

/// The tools-wide evidence adapter over the shared snapshot store.
#[derive(Debug)]
pub struct SnapshotEvidence {
    store: Arc<SnapshotStore>,
}

impl SnapshotEvidence {
    /// Wraps the snapshot store the read, search, and patch tools share.
    #[must_use]
    pub fn new(store: Arc<SnapshotStore>) -> Self {
        Self { store }
    }

    /// Rebinds an intact view to `to` and records its complete rows as
    /// delivered at `at`. Returns whether the view was intact.
    #[must_use]
    pub fn deliver(&self, view: &ReadView, to: Consumer, at: u64) -> bool {
        let Some((provenance, reference, snapshot)) = self.intact(view) else {
            return false;
        };
        let Some(rows) = complete_rows(&snapshot.bytes, &view.rows) else {
            return false;
        };
        if rows.is_empty() {
            return false;
        }
        self.store.rebind(
            provenance.session,
            reference,
            provenance.consumer,
            to,
            &rows,
            at,
        )
    }

    /// Adopts `parent`'s eligible coverage of `reference` for `child`.
    ///
    /// `reference` is a bare `r<boot>.<seq>` token or a `[path@token]`
    /// header. Returns `None`, revealing nothing, when the reference is
    /// unknown, expired, foreign, or not delivered to `parent` at or before
    /// `parent_cutoff`.
    #[must_use]
    pub fn adopt_view(
        &self,
        session: SessionId,
        reference: &str,
        parent: Consumer,
        parent_cutoff: u64,
        child: Consumer,
    ) -> Option<ReadView> {
        let (path, token) = match reference
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            Some(header) => {
                let (path, token) = header.rsplit_once('@')?;
                (Some(path), token)
            }
            None => (None, reference),
        };
        let reference = ReadRef::parse(token)?;
        let captured = self.store.snapshot(reference)?;
        if path.is_some_and(|path| Path::new(path) != captured.path) {
            return None;
        }
        let (snapshot, intervals) =
            self.store
                .adopt(session, reference, parent, parent_cutoff, child)?;
        let (rows, whole) = interval_rows(&snapshot.bytes, &intervals);
        let path = snapshot.path.to_string_lossy();
        Some(ReadView {
            header: header(&path, reference),
            path: path.into(),
            rows,
            truncated: !whole,
            provenance: Some(Provenance {
                reference: reference.display(),
                consumer: child,
                session,
            }),
        })
    }

    fn intact<'v>(&self, view: &'v ReadView) -> Option<(&'v Provenance, ReadRef, Snapshot)> {
        let provenance = view.provenance.as_ref()?;
        let reference = ReadRef::parse(&provenance.reference)?;
        let snapshot = self.store.snapshot(reference)?;
        let path = snapshot.path.to_string_lossy();
        let bound = snapshot.session == provenance.session
            && *view.path == *path
            && view.header == header(&path, reference);
        bound.then_some((provenance, reference, snapshot))
    }
}

impl Evidence for SnapshotEvidence {
    fn delivered(&self, view: &ReadView, to: Consumer, at: u64) {
        self.deliver(view, to, at);
    }

    fn adopt(
        &self,
        session: SessionId,
        reference: &str,
        parent: Consumer,
        parent_cutoff: u64,
        child: Consumer,
    ) -> Result<ReadView, AdoptError> {
        self.adopt_view(session, reference, parent, parent_cutoff, child)
            .ok_or(AdoptError::ObservationUnavailable)
    }
}

/// Who a capture is bound to: the calling session, generation, and consumer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Binding {
    pub(crate) session: SessionId,
    pub(crate) generation: GenerationId,
    pub(crate) consumer: Consumer,
}

/// The header and provenance one capture binds; views of the capture
/// repeat both.
#[derive(Clone, Debug)]
pub(crate) struct Bound {
    header: Box<str>,
    provenance: Option<Provenance>,
}

impl Bound {
    /// A view of `rows` from this capture.
    pub(crate) fn view(&self, path: &str, rows: Box<[SourceRow]>, truncated: bool) -> ReadView {
        ReadView {
            path: path.into(),
            header: self.header.clone(),
            rows,
            truncated,
            provenance: self.provenance.clone(),
        }
    }

    /// A header without a reference for a source no store may capture.
    pub(crate) fn unbound(path: &str) -> Self {
        Self {
            header: format!("[{path}]").into(),
            provenance: None,
        }
    }
}

/// Captures `bytes` at `path` for the binding with the ascending line
/// numbers the read displayed; the result has no provenance when the store
/// cannot mint a reference.
pub(crate) fn capture(
    store: &SnapshotStore,
    binding: Binding,
    path: &str,
    bytes: &[u8],
    shown: impl IntoIterator<Item = u64>,
) -> Bound {
    let Binding {
        session,
        generation,
        consumer,
    } = binding;
    match store.capture(session, generation, consumer, Path::new(path), bytes) {
        Some((reference, _)) => {
            let mut run: Option<(u64, u64)> = None;
            for line in shown {
                run = match run {
                    Some((first, last)) if last + 1 == line => Some((first, line)),
                    Some((first, last)) => {
                        store.show(reference, consumer, first, last);
                        Some((line, line))
                    }
                    None => Some((line, line)),
                };
            }
            if let Some((first, last)) = run {
                store.show(reference, consumer, first, last);
            }
            Bound {
                header: header(path, reference),
                provenance: Some(Provenance {
                    reference: reference.display(),
                    consumer,
                    session,
                }),
            }
        }
        None => Bound::unbound(path),
    }
}

/// The exact text of one physical line: lossy UTF-8 without its LF or CRLF.
pub(crate) fn line_text(raw: &[u8]) -> Cow<'_, str> {
    let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
    String::from_utf8_lossy(raw)
}

/// The `[path@reference]` header a Hashline patch copies.
fn header(path: &str, reference: ReadRef) -> Box<str> {
    format!("[{path}@{}]", reference.display()).into()
}

/// The physical lines of `bytes`, one-based, without terminators.
fn lines(bytes: &[u8]) -> impl Iterator<Item = (u64, &[u8])> {
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let pieces = (!bytes.is_empty()).then(|| body.split(|&byte| byte == b'\n'));
    (1_u64..).zip(pieces.into_iter().flatten())
}

/// Verifies every complete row against the captured lines and returns the
/// complete rows as intervals; `None` when any row is out of order, out of
/// range, or differs from the capture.
fn complete_rows(bytes: &[u8], rows: &[SourceRow]) -> Option<Vec<(u64, u64)>> {
    let mut source = lines(bytes);
    let mut intervals: Vec<(u64, u64)> = Vec::new();
    let mut previous = 0_u64;
    for row in rows {
        if row.line <= previous {
            return None;
        }
        previous = row.line;
        let (_, raw) = source.find(|&(number, _)| number == row.line)?;
        if !row.complete {
            continue;
        }
        if line_text(raw) != *row.text {
            return None;
        }
        match intervals.last_mut() {
            Some(last) if last.1 + 1 == row.line => last.1 = row.line,
            _ => intervals.push((row.line, row.line)),
        }
    }
    Some(intervals)
}

/// Complete rows for `intervals` and whether they cover the whole file.
fn interval_rows(bytes: &[u8], intervals: &[(u64, u64)]) -> (Box<[SourceRow]>, bool) {
    let mut total = 0_u64;
    let mut rows = Vec::new();
    for (number, raw) in lines(bytes) {
        total = number;
        if intervals
            .iter()
            .any(|&(first, last)| (first..=last).contains(&number))
        {
            rows.push(SourceRow {
                line: number,
                text: line_text(raw).into(),
                complete: true,
            });
        }
    }
    let whole = total == 0 || intervals == [(1, total)];
    (rows.into(), whole)
}
