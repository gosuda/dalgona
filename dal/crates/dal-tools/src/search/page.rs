//! Typed search pages served beside the display text (R06).
//!
//! Grep pages bind each hit to an intact one-row source view of the verified
//! buffer; find and symbol pages keep their own schemas and carry no
//! provenance.

use dal_core::{FindPage, SearchHit, SearchPage, SourceRow, SymbolPage, ToolData};

use crate::evidence::{Binding, Bound, capture};
use crate::patch::snapshot::SnapshotStore;

/// The typed page of one served search, before grep sources are captured.
pub(crate) enum Page {
    Grep(GrepDraft),
    Find(FindPage),
    Symbols(SymbolPage),
}

/// Grep hits over the verified buffers they were displayed from.
pub(crate) struct GrepDraft {
    pub(crate) files: Vec<DraftFile>,
    pub(crate) hits: Vec<DraftHit>,
    pub(crate) truncated: bool,
}

/// One verified buffer with displayed hits.
pub(crate) struct DraftFile {
    /// The row path, workspace-relative when `in_workspace`.
    pub(crate) shown: String,
    pub(crate) in_workspace: bool,
    pub(crate) bytes: Vec<u8>,
    pub(crate) lines: u64,
}

/// One displayed match row.
pub(crate) struct DraftHit {
    pub(crate) file: usize,
    /// The displayed, possibly cut, match text.
    pub(crate) text: String,
    pub(crate) row: SourceRow,
}

impl Page {
    /// Captures grep sources for `binding` and returns the served data.
    pub(crate) fn finish(self, store: &SnapshotStore, binding: Binding) -> ToolData {
        match self {
            Self::Grep(draft) => ToolData::Search(draft.finish(store, binding)),
            Self::Find(page) => ToolData::Find(page),
            Self::Symbols(page) => ToolData::Symbols(page),
        }
    }
}

impl GrepDraft {
    fn finish(self, store: &SnapshotStore, binding: Binding) -> SearchPage {
        let bounds: Vec<Bound> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                if !file.in_workspace {
                    return Bound::unbound(&file.shown);
                }
                let mut lines: Vec<u64> = self
                    .hits
                    .iter()
                    .filter(|hit| hit.file == index && hit.row.complete)
                    .map(|hit| hit.row.line)
                    .collect();
                lines.sort_unstable();
                lines.dedup();
                if lines.is_empty() {
                    return Bound::unbound(&file.shown);
                }
                capture(store, binding, &file.shown, &file.bytes, lines)
            })
            .collect();
        let matches = self
            .hits
            .into_iter()
            .map(|hit| {
                let file = &self.files[hit.file];
                let whole = file.lines == 1 && hit.row.line == 1;
                SearchHit {
                    path: file.shown.as_str().into(),
                    line: hit.row.line,
                    text: hit.text.into(),
                    source: bounds[hit.file].view(&file.shown, Box::new([hit.row]), !whole),
                }
            })
            .collect();
        SearchPage {
            matches,
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use dal_core::{Consumer, SourceRow, ToolData};

    use super::super::tests::{engine, host, write};
    use crate::evidence::SnapshotEvidence;
    use crate::patch::snapshot::ReadRef;

    #[tokio::test]
    async fn grep_hits_carry_intact_one_row_sources() {
        let dir = tempfile::tempdir().expect("temp workspace");
        let long = "needle".repeat(100);
        write(
            dir.path(),
            "a.txt",
            format!("alpha needle\r\nbeta\n{long}\n").as_bytes(),
        );
        let search = engine(false, None);
        let call = host(dir.path());
        let (_, data) = search
            .execute(r#"{"mode":"grep","pattern":"needle"}"#, &call)
            .await
            .expect("grep runs");
        let ToolData::Search(page) = data else {
            panic!("grep serves a search page, got {data:?}");
        };
        assert!(!page.truncated);
        let rows: Vec<(&str, u64, &[SourceRow])> = page
            .matches
            .iter()
            .map(|hit| (hit.path.as_ref(), hit.line, &*hit.source.rows))
            .collect();
        let cut = format!("{}...", &long[..500]);
        assert_eq!(
            rows,
            [
                (
                    "a.txt",
                    1,
                    &[SourceRow {
                        line: 1,
                        text: "alpha needle".into(),
                        complete: true
                    }][..]
                ),
                (
                    "a.txt",
                    3,
                    &[SourceRow {
                        line: 3,
                        text: cut.as_str().into(),
                        complete: false
                    }][..]
                ),
            ]
        );
        let evidence = SnapshotEvidence::new(search.snapshots.clone());
        assert!(evidence.deliver(&page.matches[0].source, Consumer::Model, 1));
        assert!(!evidence.deliver(&page.matches[1].source, Consumer::Model, 1));
        let provenance = page.matches[0]
            .source
            .provenance
            .as_ref()
            .expect("workspace hits are captured");
        let reference = ReadRef::parse(&provenance.reference).expect("reference");
        assert_eq!(page.matches[1].source.header, page.matches[0].source.header);
        assert_eq!(
            search
                .snapshots
                .delivered(call.session, Consumer::Model, reference),
            [(1, 1)]
        );
        assert!(
            search
                .snapshots
                .lookup(
                    call.session,
                    call.generation,
                    Consumer::Model,
                    reference,
                    Path::new("a.txt")
                )
                .is_some()
        );
    }
}
