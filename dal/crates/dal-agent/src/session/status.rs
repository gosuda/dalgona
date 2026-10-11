//! Extension status polling: one bounded, synchronous sweep per actor tick.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use dal_core::{ExtState, ExtStatus, SessionId};

use crate::ext::generation::Generation;
use crate::ext::{ExtRecord, StatusCx};

/// Interval between status sweeps; every poll is a bounded synchronous read.
pub(crate) const STATUS_POLL: Duration = Duration::from_millis(50);

/// Longest status text carried on an update, in bytes.
const STATUS_TEXT_MAX: usize = 4096;

/// Polls every registered status kind and returns the statuses that differ
/// from `published`, including a quiet reset for any extension that has left
/// the generation.
///
/// An extension with no published entry is quiet with no text, so an
/// unchanged quiet poll produces nothing and a burst of identical busy polls
/// produces one status.
pub(crate) fn sweep(
    session: SessionId,
    generation: &Generation,
    rows: &[ExtRecord],
    published: &BTreeMap<Box<str>, ExtStatus>,
) -> Vec<ExtStatus> {
    if generation.status_kinds().is_empty() && published.is_empty() {
        return Vec::new();
    }
    let mut changes = Vec::new();
    let mut registered = BTreeSet::new();
    for extension in generation.extensions.iter() {
        let Some((_, poll)) = extension.status() else {
            continue;
        };
        let ext: Box<str> = extension.name().into();
        registered.insert(ext.clone());
        let grouped = grouped_records(rows, &ext);
        let snapshot = poll.snapshot(&StatusCx::new(session, &grouped));
        let fresh = ExtStatus {
            ext: ext.clone(),
            state: if snapshot.quiet {
                ExtState::Quiet
            } else {
                ExtState::Busy
            },
            text: snapshot.text.map(clamp_text),
        };
        if published.get(&ext).is_some_and(|held| *held == fresh) {
            continue;
        }
        if !published.contains_key(&ext) && is_baseline(&fresh) {
            continue;
        }
        changes.push(fresh);
    }
    for ext in published.keys() {
        if !registered.contains(ext) {
            changes.push(ExtStatus {
                ext: ext.clone(),
                state: ExtState::Quiet,
                text: None,
            });
        }
    }
    changes
}

/// Whether a status is the quiet, textless state every extension starts in.
pub(crate) fn is_baseline(status: &ExtStatus) -> bool {
    status.is_quiet() && status.text.is_none()
}

fn grouped_records(rows: &[ExtRecord], ext: &str) -> BTreeMap<Box<str>, Vec<ExtRecord>> {
    let mut grouped: BTreeMap<Box<str>, Vec<ExtRecord>> = BTreeMap::new();
    for row in rows.iter().filter(|row| row.ext.as_str() == ext) {
        grouped
            .entry(row.kind.clone())
            .or_default()
            .push(row.clone());
    }
    grouped
}

fn clamp_text(text: Box<str>) -> Box<str> {
    if text.len() <= STATUS_TEXT_MAX {
        return text;
    }
    let mut end = STATUS_TEXT_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use dal_core::{ExtState, ExtStatus, ServiceSet, SessionId};

    use super::{STATUS_TEXT_MAX, sweep};
    use crate::ext::generation::{Generation, ValidatedExtensions};
    use crate::ext::{ExtensionBuilder, StatusCx, StatusPoll, StatusSnapshot};

    struct Fixed(bool, Option<String>);

    impl StatusPoll for Fixed {
        fn snapshot(&self, _cx: &StatusCx) -> StatusSnapshot {
            StatusSnapshot {
                quiet: self.0,
                text: self.1.clone().map(Into::into),
            }
        }
    }

    fn generation(poll: Option<Fixed>) -> Generation {
        let mut builder =
            ExtensionBuilder::new("focus", "0.1.0", ServiceSet::EMPTY).expect("builder");
        if let Some(poll) = poll {
            builder = builder.status_kind("focus", Arc::new(poll));
        }
        let extension = builder.build().expect("extension");
        Generation::build(ValidatedExtensions::validate(vec![extension], None).expect("valid"))
    }

    fn busy(text: Option<&str>) -> ExtStatus {
        ExtStatus {
            ext: "focus".into(),
            state: ExtState::Busy,
            text: text.map(Into::into),
        }
    }

    #[test]
    fn unchanged_polls_publish_nothing_and_quiet_baseline_is_silent() {
        let session = SessionId::new_v7();
        let quiet = generation(Some(Fixed(true, None)));
        assert_eq!(sweep(session, &quiet, &[], &BTreeMap::new()), []);

        let working = generation(Some(Fixed(false, Some("indexing".to_owned()))));
        let first = sweep(session, &working, &[], &BTreeMap::new());
        assert_eq!(first, vec![busy(Some("indexing"))]);
        let held = BTreeMap::from([("focus".into(), busy(Some("indexing")))]);
        assert_eq!(sweep(session, &working, &[], &held), []);
    }

    #[test]
    fn departed_extension_resets_to_quiet_and_long_text_is_clamped() {
        let session = SessionId::new_v7();
        let held = BTreeMap::from([("focus".into(), busy(None))]);
        let without = generation(None);
        assert_eq!(
            sweep(session, &without, &[], &held),
            vec![ExtStatus {
                ext: "focus".into(),
                state: ExtState::Quiet,
                text: None,
            }]
        );

        let noisy = generation(Some(Fixed(false, Some("é".repeat(STATUS_TEXT_MAX)))));
        let changes = sweep(session, &noisy, &[], &BTreeMap::new());
        let text = changes[0].text.as_deref().expect("text kept");
        assert!(text.len() <= STATUS_TEXT_MAX);
        assert!(text.chars().all(|character| character == 'é'));
    }
}
