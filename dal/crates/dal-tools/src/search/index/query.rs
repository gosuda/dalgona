//! Candidate lookup and the indexed find over a validated snapshot.

use super::store::{Postings, next_bit, pack};

/// Necessary conditions for `literal` to occur: gram `i + 1` sits one byte
/// after some occurrence of gram `i`, and gram `i` is followed by byte `i + 3`.
fn adjacent(masks: &[(u8, u8)], literal: &[u8]) -> bool {
    masks
        .windows(2)
        .all(|pair| pair[0].0.rotate_left(1) & pair[1].0 != 0)
        && masks.iter().enumerate().all(|(at, (_, next))| {
            literal
                .get(at + 3)
                .is_none_or(|byte| next & next_bit(*byte) != 0)
        })
}

impl Postings {
    /// Files whose postings prove every gram of `literal` present, adjacent by
    /// location mask, and followed by the right byte class by next mask.
    /// `None` marks the section corrupt: the caller must take the fallback.
    #[must_use]
    pub(super) fn literal_files(&self, literal: &[u8], files: usize) -> Option<Vec<u32>> {
        let mut lists = Vec::with_capacity(literal.len().saturating_sub(2));
        for window in literal.windows(3) {
            match self.checked_list(pack(window[0], window[1], window[2]), files) {
                Some(super::store::GramList::List(start, len)) => lists.push((start, len)),
                // An absent gram proves no file holds the literal.
                Some(super::store::GramList::Absent) => return Some(Vec::new()),
                None => return None,
            }
        }
        let Some(&(start, len)) = lists.iter().min_by_key(|(_, len)| *len) else {
            return Some(Vec::new());
        };
        let mut masks = vec![(0_u8, 0_u8); lists.len()];
        let mut out = Vec::new();
        for at in start..start + len {
            let (file, _, _) = self.record(at)?;
            let mut present = true;
            for (list, slot) in lists.iter().zip(masks.iter_mut()) {
                if let Some(found) = self.masks(*list, file) {
                    *slot = found;
                } else {
                    present = false;
                    break;
                }
            }
            if present && adjacent(&masks, literal) {
                out.push(file);
            }
        }
        Some(out)
    }
}

pub(super) fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    while let (Some(x), Some(y)) = (a.get(i), b.get(j)) {
        match x.cmp(y) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(*x);
                i += 1;
                j += 1;
            }
        }
    }
    out
}
