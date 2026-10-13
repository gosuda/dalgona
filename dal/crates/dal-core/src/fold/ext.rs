use std::collections::BTreeSet;

use super::{Entry, EntryId, RawJson, Session};

/// One extension record with the entry it rides on.
///
/// The anchor is the tree leaf at the moment the record entered the fold,
/// so a record is visible exactly while the current root-to-leaf path
/// contains its anchor. A record written before any entry has no anchor and
/// is visible on every branch.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ExtRow {
    anchor: Option<EntryId>,
    ext: Box<str>,
    kind: Box<str>,
    body: RawJson,
}

/// One extension record visible on the current leaf path.
#[derive(Clone, Copy, Debug)]
pub struct LeafExt<'a> {
    /// The contributing extension.
    pub ext: &'a str,
    /// The extension-defined record kind.
    pub kind: &'a str,
    /// The opaque record body.
    pub body: &'a RawJson,
}

impl Session {
    /// Folds one extension record onto the current leaf.
    ///
    /// Replay and the live actor both call this once per journaled
    /// `Record::Ext`, so the anchor rule has one owner.
    pub fn fold_ext(&mut self, ext: &str, kind: &str, body: &RawJson) {
        self.ext_rows.push(ExtRow {
            anchor: self.tree.leaf,
            ext: ext.into(),
            kind: kind.into(),
            body: body.clone(),
        });
    }

    /// Returns the extension records on the current root-to-leaf path in
    /// journal order.
    pub fn leaf_ext(&self) -> impl Iterator<Item = LeafExt<'_>> {
        let path: BTreeSet<EntryId> = self.tree.ancestors(self.tree.leaf).into_iter().collect();
        self.ext_rows
            .iter()
            .filter(move |row| row.anchor.is_none_or(|anchor| path.contains(&anchor)))
            .map(|row| LeafExt {
                ext: &row.ext,
                kind: &row.kind,
                body: &row.body,
            })
    }

    /// Returns the id the next tree entry will take, which is also the
    /// journal position of an extension record folded now.
    #[must_use]
    pub const fn next_entry_id(&self) -> Option<EntryId> {
        self.next_entry
    }

    /// Returns the number of extension records ever folded; a leaf move or an
    /// append are the only changes to the visible set, so the pair of this
    /// count and the current leaf identifies it.
    #[must_use]
    pub const fn ext_len(&self) -> usize {
        self.ext_rows.len()
    }

    /// Returns the current leaf entry, if any.
    #[must_use]
    pub const fn leaf_entry(&self) -> Option<EntryId> {
        self.tree.leaf
    }

    /// Returns the entries on the current root-to-leaf path, root first.
    #[must_use]
    pub fn leaf_entries(&self) -> Vec<&Entry> {
        self.tree
            .ancestors(self.tree.leaf)
            .iter()
            .filter_map(|id| self.tree.entries.get(id))
            .collect()
    }
}
