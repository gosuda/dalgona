//! Run grants an approved tool call lends to its extension's `run` calls.
//!
//! A tool whose classification carries a [`dal_core::GrantSpec`] and whose
//! call was approved earns one grant. The extension's own `run` service
//! calls made for that call ride the grant with no further asks, for the
//! argv prefix and roots it names. The grant lasts while the call is in
//! flight and then until the one run job that call started has ended. It
//! never serves another call, another extension, or another argv, so a run
//! that was not approved gets nothing from a sibling's approval.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use dal_core::{JobId, Name, SessionId};

use crate::ext::Caller;
use crate::jobs::JobTable;

/// One host-minted tool-call invocation. A provider's call id repeats across
/// rounds and turns, so grants never key on it: every dispatched call gets a
/// fresh token that no later call can reuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Invocation(u64);

impl Invocation {
    /// Mints a token no other invocation of this process shares.
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// The identity a run grant binds to: the extension that owns the tool and
/// the invocation that earned it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CallKey {
    ext: Name,
    invocation: Invocation,
}

impl CallKey {
    /// The key of one dispatched call.
    pub(crate) fn new(ext: Name, invocation: Invocation) -> Self {
        Self { ext, invocation }
    }

    /// The key of the tool call a caller was minted for, if it came from one.
    pub(crate) fn of(who: &Caller) -> Option<Self> {
        Some(Self::new(who.ext().clone(), who.invocation()?))
    }
}

/// One approved call's grant.
struct ServiceGrant {
    key: CallKey,
    prefix: Box<[OsString]>,
    roots: Box<[PathBuf]>,
    /// Whether the approving call is still running.
    in_flight: bool,
    /// The one run job the call started, bound once while the call ran.
    job: Option<JobId>,
}

impl ServiceGrant {
    /// A grant outlives its call only through its run job while that has
    /// not ended.
    fn alive(&self, jobs: &JobTable) -> bool {
        self.job.map_or(self.in_flight, |job| jobs.is_live(job))
    }
}

/// The scope one covering grant lends a run call.
pub(crate) struct Covering {
    /// The argv prefix the spawn must start with.
    pub(crate) prefix: Box<[OsString]>,
    /// The roots the spawn's working directory must stay inside.
    pub(crate) roots: Box<[PathBuf]>,
    pub(crate) job: Option<JobId>,
}

/// The session's registry of live run grants. It dies with the session.
#[derive(Default)]
pub(crate) struct ServiceGrants(Mutex<Vec<ServiceGrant>>);

impl ServiceGrants {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<ServiceGrant>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records the grant an approved call earned, replacing an earlier one
    /// the same call earned.
    pub(crate) fn register(&self, key: CallKey, prefix: Box<[OsString]>, roots: Box<[PathBuf]>) {
        let mut grants = self.lock();
        grants.retain(|grant| grant.key != key);
        grants.push(ServiceGrant {
            key,
            prefix,
            roots,
            in_flight: true,
            job: None,
        });
    }

    /// Binds the one run job the approving call started. Only a call still
    /// in flight binds, and only once: a later spawn by a retained caller
    /// cannot revive an ended grant.
    pub(crate) fn bind_job(&self, key: &CallKey, job: JobId) {
        let mut grants = self.lock();
        let Some(grant) = grants.iter_mut().find(|grant| grant.key == *key) else {
            return;
        };
        if grant.in_flight && grant.job.is_none() {
            grant.job = Some(job);
        }
    }

    /// Marks the approving call finished; its run job now bounds the grant.
    pub(crate) fn call_ended(&self, key: &CallKey) {
        let mut grants = self.lock();
        for grant in grants.iter_mut().filter(|grant| grant.key == *key) {
            grant.in_flight = false;
        }
        grants.retain(|grant| grant.in_flight || grant.job.is_some());
    }

    /// Rechecks a lent proof at the synchronous process-spawn boundary.
    pub(crate) fn is_live(&self, key: &CallKey, jobs: &JobTable) -> bool {
        self.lock()
            .iter()
            .any(|grant| grant.key == *key && grant.alive(jobs))
    }

    /// Returns the grant that covers one `run` call, if any: it must belong
    /// to the same call, be alive, and cover both the argv prefix and the
    /// working directory. Dead grants are dropped.
    pub(crate) fn cover(
        &self,
        key: &CallKey,
        argv: &[OsString],
        cwd: &Path,
        jobs: &JobTable,
    ) -> Option<Covering> {
        let mut grants = self.lock();
        grants.retain(|grant| grant.alive(jobs));
        grants
            .iter()
            .find(|grant| {
                grant.key == *key
                    && starts_with_prefix(&grant.prefix, argv)
                    && crate::proc::cwd_in_roots(cwd, &grant.roots)
            })
            .map(|grant| Covering {
                prefix: grant.prefix.clone(),
                roots: grant.roots.clone(),
                job: grant.job,
            })
    }
}

/// Whether `argv` starts with every prefix token; an empty prefix covers
/// nothing.
fn starts_with_prefix(prefix: &[OsString], argv: &[OsString]) -> bool {
    !prefix.is_empty()
        && argv.len() >= prefix.len()
        && prefix.iter().zip(argv).all(|(want, got)| want == got)
}

/// Keeps a grant out of other sessions' data. A root that sits directly
/// under the host data root names a tree every session shares, such as the
/// worktree and isolation roots, so the grant keeps only this session's
/// child of it. Every other root stays as approved.
pub(crate) fn scope_roots(
    roots: Vec<PathBuf>,
    data_root: &Path,
    workspace: &Path,
    session: SessionId,
) -> Box<[PathBuf]> {
    let data_root = data_root
        .canonicalize()
        .unwrap_or_else(|_| data_root.to_path_buf());
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    roots
        .into_iter()
        .map(|root| {
            let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
            if workspace == canonical_root {
                return root;
            }
            let parent = root.parent().map(|parent| {
                parent
                    .canonicalize()
                    .unwrap_or_else(|_| parent.to_path_buf())
            });
            if parent.as_deref() == Some(data_root.as_path()) {
                root.join(session.to_string())
            } else {
                root
            }
        })
        .collect()
}
