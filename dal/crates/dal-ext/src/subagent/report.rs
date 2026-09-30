use dal_core::{AgentReport, EntryId, SessionId};

use super::MAX_REPORT_CHARS;

/// Points at the child's original final journal entry; never a second store.
pub(crate) fn report_uri(session: &SessionId, entry: &EntryId) -> String {
    format!("session://{session}/{entry}")
}

/// Bounds a child report at 50,000 Unicode scalar values with a spill line.
pub(crate) fn bounded_report(mut report: String, session_uri: &str) -> String {
    let Some((byte_end, _)) = report.char_indices().nth(MAX_REPORT_CHARS) else {
        return report;
    };

    report.truncate(byte_end);
    let spill = format!(
        "\n[report truncated at 50000 characters; full report: {session_uri}]\nnext_step: Read the full report with the read tool at {session_uri}."
    );
    report.reserve(spill.len());
    report.push_str(&spill);
    report
}

/// Renders a completed child report with its journal-entry spill link.
pub(crate) fn report_text(report: AgentReport) -> String {
    let uri = report_uri(&report.session, &report.entry);
    bounded_report(report.text.into(), &uri)
}
