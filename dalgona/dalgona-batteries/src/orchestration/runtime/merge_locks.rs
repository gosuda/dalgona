// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

type Entries = Mutex<HashMap<PathBuf, Weak<Slot>>>;

/// The test probe of one canonical workspace key: how many merges hold it
/// right now and the most that ever held it at once. It keeps one identity
/// per key across slot churn, so a peak read after the merges end still
/// tells how far that workspace's serialization stretched.
#[cfg(test)]
#[derive(Default)]
struct Probe {
    holds: AtomicUsize,
    peak: AtomicUsize,
}

#[cfg(test)]
type Probes = Mutex<HashMap<PathBuf, Arc<Probe>>>;

/// The merge locks of the workspaces a runtime hosts. An entry lives only
/// while a merge holds or awaits its workspace, so the registry never grows
/// with the sessions that never merge.
#[derive(Clone, Default)]
pub(super) struct MergeLocks {
    entries: Arc<Entries>,
    /// The test probes, one per canonical workspace key. The counters climb
    /// only on real acquisition and fall on the RAII drop, so a cancelled or
    /// failed merge carries them back to zero like any released hold.
    #[cfg(test)]
    probes: Arc<Probes>,
}

/// One workspace's lock identity; the registry forgets it when its last
/// holder drops.
struct Slot {
    gate: Arc<tokio::sync::Mutex<()>>,
    key: PathBuf,
    entries: Arc<Entries>,
}

/// Exclusive right to apply patches to one workspace checkout. The probe of
/// the checkout's canonical key drops before the gate guard releases, so a
/// waiter that acquires next is never charged for this hold by mistake.
pub(super) struct MergeHold {
    #[cfg(test)]
    probe: Arc<Probe>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
    _slot: Arc<Slot>,
}

impl MergeLocks {
    /// Waits for the one lock of the checkout at `workspace`. Two paths that
    /// resolve to the same directory share it; the resolution runs off the
    /// async executor.
    pub(super) async fn hold(&self, workspace: &Path) -> MergeHold {
        let key = tokio::fs::canonicalize(workspace)
            .await
            .unwrap_or_else(|_| workspace.to_path_buf());
        #[cfg(test)]
        let probe = self.probe(&key);
        let slot = self.slot(key);
        let guard = Arc::clone(&slot.gate).lock_owned().await;
        #[cfg(test)]
        {
            let holds = probe.holds.fetch_add(1, Ordering::SeqCst) + 1;
            probe.peak.fetch_max(holds, Ordering::SeqCst);
        }
        MergeHold {
            #[cfg(test)]
            probe,
            _guard: guard,
            _slot: slot,
        }
    }

    /// The probe of one canonical workspace key, created on first use. The
    /// probe outlives the slot so peaks stay readable after the last merge
    /// of a test drops its hold.
    #[cfg(test)]
    fn probe(&self, key: &Path) -> Arc<Probe> {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key.to_path_buf())
            .or_default()
            .clone()
    }

    /// How many merges hold one canonical workspace key right now.
    #[cfg(test)]
    pub(super) fn key_holds(&self, key: &Path) -> usize {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .map_or(0, |probe| probe.holds.load(Ordering::SeqCst))
    }

    /// The most merges that ever held one canonical workspace key at once.
    /// Under the scale load this never goes above 1: merges into one
    /// workspace run one at a time.
    #[cfg(test)]
    pub(super) fn key_peak(&self, key: &Path) -> usize {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .map_or(0, |probe| probe.peak.load(Ordering::SeqCst))
    }

    /// How many canonical keys hold a merge right now, across every
    /// checkout. Two sessions on unrelated checkouts may both read nonzero
    /// here at once; a global lock would pin one of them and stall an
    /// independent merge.
    #[cfg(test)]
    pub(super) fn distinct_holds(&self) -> usize {
        self.probes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .filter(|probe| probe.holds.load(Ordering::SeqCst) > 0)
            .count()
    }

    /// The canonical key a workspace path maps to, resolved the same way the
    /// hold path resolves it. Tests read probes through this so symlinked
    /// spellings attribute to the one key the lock actually serializes on.
    #[cfg(test)]
    pub(super) async fn canonical_key(workspace: &Path) -> PathBuf {
        tokio::fs::canonicalize(workspace)
            .await
            .unwrap_or_else(|_| workspace.to_path_buf())
    }

    fn slot(&self, key: PathBuf) -> Arc<Slot> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = entries.get(&key).and_then(Weak::upgrade) {
            return slot;
        }
        let slot = Arc::new(Slot {
            gate: Arc::default(),
            key: key.clone(),
            entries: Arc::clone(&self.entries),
        });
        entries.insert(key, Arc::downgrade(&slot));
        slot
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries
            .get(&self.key)
            .is_some_and(|slot| slot.strong_count() == 0)
        {
            entries.remove(&self.key);
        }
    }
}

/// Returns the checkout's probe count when the hold ends, however it ends:
/// a merge the owner cancels or that fails down the retain path unwinds
/// through this drop exactly like a released hold, so no stranded count can
/// wedge the next merge's accounting.
#[cfg(test)]
impl Drop for MergeHold {
    fn drop(&mut self) {
        self.probe.holds.fetch_sub(1, Ordering::SeqCst);
    }
}
