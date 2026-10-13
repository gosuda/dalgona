// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, Weak};

type Entries = Mutex<HashMap<PathBuf, Weak<Slot>>>;

/// The merge locks of the workspaces a runtime hosts. An entry lives only
/// while a merge holds or awaits its workspace, so the registry never grows
/// with the sessions that never merge.
#[derive(Clone, Default)]
pub(super) struct MergeLocks {
    entries: Arc<Entries>,
}

/// One workspace's lock identity; the registry forgets it when its last
/// holder drops.
struct Slot {
    gate: Arc<tokio::sync::Mutex<()>>,
    key: PathBuf,
    entries: Arc<Entries>,
}

/// Exclusive right to apply patches to one workspace checkout.
pub(super) struct MergeHold {
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
        let slot = self.slot(key);
        let guard = Arc::clone(&slot.gate).lock_owned().await;
        MergeHold {
            _guard: guard,
            _slot: slot,
        }
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
