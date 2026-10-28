//! Adverse recovery of `reports.jsonl`. Every test appends through the real
//! `JobLedger::append` into a real temporary file, leaves the file the way
//! an interrupted append can, reopens through the real ledger opening, and
//! compares the recovered reports by id and content.
//!
//! No test injects an I/O failure: `append` takes no seam that fails it
//! after some of its bytes have landed, and the tests do not fake one. The
//! tests whose names start with `state_fixture_` lay down the disk bytes
//! such a failure can leave and prove what a reopen and the next append
//! must do with those bytes.

use std::path::Path;

use super::*;

fn report(label: &str, text: &str) -> JobReport {
    JobReport {
        id: JobId::new_v7(),
        label: label.into(),
        outcome: JobOutcome::Exited { code: 0 },
        text: text.into(),
    }
}

fn end_lines(reports: &[&JobReport]) -> Vec<JobLedgerEntry> {
    reports.iter().copied().map(JobLedger::end).collect()
}

fn ledger_path(dir: &Path) -> PathBuf {
    dir.join(LEDGER_FILE)
}

fn file_bytes(dir: &Path) -> Vec<u8> {
    std::fs::read(ledger_path(dir)).expect("read the ledger file")
}

/// Cuts the ledger file to its first `len` bytes: what a process killed,
/// or a disk that filled, in the middle of an append leaves behind.
fn truncate_to(dir: &Path, len: usize) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(ledger_path(dir))
        .expect("open the ledger file to tear it")
        .set_len(u64::try_from(len).expect("offset fits in a file length"))
        .expect("tear the ledger tail");
}

fn append_raw(dir: &Path, bytes: &[u8]) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(ledger_path(dir))
        .expect("open the ledger file to damage it")
        .write_all(bytes)
        .expect("append the damaged bytes");
}

/// The text after the last newline of `bytes`: the unterminated tail.
fn unterminated_tail(bytes: &[u8]) -> String {
    let start = bytes
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |index| index + 1);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// Asserts that `id` is named on exactly one physical line of the file
/// and that the line is one whole end record for `id`, not a record
/// merged with the bytes that preceded it.
fn assert_one_ledger_record(dir: &Path, id: JobId) {
    let text = String::from_utf8_lossy(&file_bytes(dir)).into_owned();
    let needle = id.to_string();
    let named: Vec<&str> = text
        .lines()
        .filter(|line| line.contains(needle.as_str()))
        .collect();
    assert_eq!(
        named.len(),
        1,
        "{id} must be on exactly one line of:\n{text}"
    );
    match sonic_rs::from_str::<Line>(named[0]) {
        Ok(Line::End { id: written, .. }) => {
            assert_eq!(written, id, "the line holds another report's record");
        }
        other => panic!(
            "{id} was appended into a line that is not one ledger record: {:?} -> {other:?}",
            named[0]
        ),
    }
}

async fn reopen(dir: &Path, stage: &str) -> (JobLedger, JobLedgerRecovery) {
    JobLedger::open(dir.to_path_buf(), dir.to_path_buf(), &HashSet::new())
        .await
        .unwrap_or_else(|error| panic!("reopen the ledger {stage}: {error:?}"))
}

#[tokio::test]
async fn torn_json_tail_then_next_append_keeps_every_acknowledged_report() {
    let dir = tempfile::tempdir().expect("temporary ledger directory");
    let acknowledged = report("acknowledged", "first report");
    let torn = report("torn", "second report");
    let next = report("next", "third report");

    let (mut ledger, recovered) = reopen(dir.path(), "of an empty directory").await;
    assert_eq!(recovered.ended, Vec::<JobReport>::new());
    ledger
        .append(&end_lines(&[&acknowledged]))
        .await
        .expect("append the acknowledged report");
    let acknowledged_len = file_bytes(dir.path()).len();
    ledger
        .append(&end_lines(&[&torn]))
        .await
        .expect("append the report that is then torn");
    let full_len = file_bytes(dir.path()).len();
    drop(ledger);
    truncate_to(
        dir.path(),
        acknowledged_len + (full_len - acknowledged_len) / 2,
    );
    assert!(
        sonic_rs::from_str::<Line>(&unterminated_tail(&file_bytes(dir.path()))).is_err(),
        "precondition: the cut leaves an unterminated line that is not a ledger record"
    );

    let (mut ledger, recovered) = reopen(dir.path(), "after the torn append").await;
    assert_eq!(
        recovered.ended,
        vec![acknowledged.clone()],
        "the acknowledged report is a record and the torn tail is not"
    );
    assert_eq!(recovered.pending, vec![acknowledged.clone()]);

    ledger
        .append(&end_lines(&[&next]))
        .await
        .expect("append after the reopen");
    drop(ledger);
    assert_one_ledger_record(dir.path(), next.id);

    let (_, recovered) = reopen(dir.path(), "after an append that followed a torn tail").await;
    assert_eq!(recovered.ended, vec![acknowledged.clone(), next.clone()]);
    assert_eq!(recovered.pending, vec![acknowledged, next]);
}

#[tokio::test]
async fn unterminated_valid_tail_then_next_append_keeps_each_record_on_its_own_line() {
    let dir = tempfile::tempdir().expect("temporary ledger directory");
    let acknowledged = report("acknowledged", "first report");
    let unterminated = report("unterminated", "second report");
    let next = report("next", "third report");

    let (mut ledger, _) = reopen(dir.path(), "of an empty directory").await;
    ledger
        .append(&end_lines(&[&acknowledged, &unterminated]))
        .await
        .expect("append both reports");
    drop(ledger);
    let bytes = file_bytes(dir.path());
    assert_eq!(
        bytes.last(),
        Some(&b'\n'),
        "every ledger line is terminated"
    );
    truncate_to(dir.path(), bytes.len() - 1);
    assert!(
        sonic_rs::from_str::<Line>(&unterminated_tail(&file_bytes(dir.path()))).is_ok(),
        "precondition: the cut leaves a complete record without its newline"
    );

    let (mut ledger, recovered) = reopen(dir.path(), "with an unterminated final record").await;
    assert_eq!(
        recovered.ended,
        vec![acknowledged.clone(), unterminated.clone()],
        "a complete record without its newline is still a record"
    );

    ledger
        .append(&end_lines(&[&next]))
        .await
        .expect("append after the reopen");
    drop(ledger);
    assert_one_ledger_record(dir.path(), next.id);

    let (_, recovered) = reopen(
        dir.path(),
        "after an append that followed an unterminated record",
    )
    .await;
    assert_eq!(
        recovered.ended,
        vec![acknowledged.clone(), unterminated.clone(), next.clone()]
    );
    assert_eq!(recovered.pending, vec![acknowledged, unterminated, next]);
}

#[tokio::test]
async fn torn_tail_cut_inside_a_multibyte_character_still_reopens() {
    let dir = tempfile::tempdir().expect("temporary ledger directory");
    let acknowledged = report("acknowledged", "first report");
    let torn = report("torn", "報告 完了 — résumé");
    let next = report("next", "third report");

    let (mut ledger, _) = reopen(dir.path(), "of an empty directory").await;
    ledger
        .append(&end_lines(&[&acknowledged]))
        .await
        .expect("append the acknowledged report");
    let acknowledged_len = file_bytes(dir.path()).len();
    ledger
        .append(&end_lines(&[&torn]))
        .await
        .expect("append the report that is then torn");
    let lead = file_bytes(dir.path())[acknowledged_len..]
        .iter()
        .position(|&byte| byte >= 0xC0)
        .expect("the writer keeps non-ASCII report text as UTF-8 bytes");
    truncate_to(dir.path(), acknowledged_len + lead + 1);
    let cut_bytes = file_bytes(dir.path());
    let invalid = std::str::from_utf8(&cut_bytes)
        .expect_err("precondition: the cut leaves a partial character");
    assert!(
        invalid.error_len().is_none(),
        "precondition: the file ends inside a character, as an interrupted append leaves it"
    );

    let (mut ledger, recovered) = reopen(dir.path(), "after a tear inside a character").await;
    assert_eq!(
        recovered.ended,
        vec![acknowledged.clone()],
        "the acknowledged report survives a tail that is not valid UTF-8"
    );

    ledger
        .append(&end_lines(&[&next]))
        .await
        .expect("append after the reopen");
    drop(ledger);
    assert_one_ledger_record(dir.path(), next.id);

    let (_, recovered) = reopen(
        dir.path(),
        "after an append that followed a tear inside a character",
    )
    .await;
    assert_eq!(recovered.ended, vec![acknowledged.clone(), next.clone()]);
    assert_eq!(recovered.pending, vec![acknowledged, next]);
}

#[tokio::test]
async fn a_complete_invalid_line_is_corruption_not_a_torn_tail() {
    let first = report("first", "first report");
    let last = report("last", "last report");

    let middle = tempfile::tempdir().expect("temporary ledger directory");
    let (mut ledger, _) = reopen(middle.path(), "of an empty directory").await;
    ledger
        .append(&end_lines(&[&first]))
        .await
        .expect("append the first report");
    append_raw(middle.path(), b"this line is not a ledger record\n");
    // Fixture contract correction: the valid record after the damaged
    // line is laid down with a raw append, not with an acknowledged append. A
    // write over a complete invalid line must be refused as corrupt, so
    // building this file through the ledger would demand the very
    // acknowledgment the ledger must never give. The intended bytes
    // (valid, invalid, valid, each newline-terminated; the record is
    // serialized exactly as the ledger serializes it) and the
    // `Corrupt { line: 2 }` assertion below are unchanged.
    let mut suffix = sonic_rs::to_string(&JobLedger::end(&last))
        .expect("serialize the record that follows the damaged line");
    suffix.push('\n');
    append_raw(middle.path(), suffix.as_bytes());
    match JobLedger::open(
        middle.path().to_path_buf(),
        middle.path().to_path_buf(),
        &HashSet::new(),
    )
    .await
    {
        Err(JobLedgerError::Corrupt { line, .. }) => assert_eq!(line, 2),
        other => panic!("an invalid line before the tail must be corrupt at line 2: {other:?}"),
    }

    let end = tempfile::tempdir().expect("temporary ledger directory");
    let (mut ledger, _) = reopen(end.path(), "of an empty directory").await;
    ledger
        .append(&end_lines(&[&first]))
        .await
        .expect("append the first report");
    append_raw(end.path(), b"{\"e\":\"end\",\"id\":\n");
    match JobLedger::open(
        end.path().to_path_buf(),
        end.path().to_path_buf(),
        &HashSet::new(),
    )
    .await
    {
        Err(JobLedgerError::Corrupt { line, .. }) => assert_eq!(line, 2),
        other => panic!("a terminated invalid last line must be corrupt at line 2: {other:?}"),
    }
}

/// Attack: a valid End prefix, then a complete invalid line that ends in
/// its newline, appended by something other than the ledger while this
/// ledger value is live. The next `append` is the first thing to see the
/// damage. Whatever the batch holds, it must be refused with a typed
/// `Corrupt` naming the damaged line and leave every byte as found: no
/// acknowledgment over corruption already at rest.
#[tokio::test]
async fn a_write_over_a_complete_invalid_line_is_refused_as_corrupt_and_appends_nothing() {
    let first = report("first", "first report");
    let next = report("next", "next report");
    let batches = [
        ("an end batch", end_lines(&[&next])),
        ("a hold batch", vec![JobLedger::hold(next.id)]),
    ];

    for (shape, batch) in batches {
        let dir = tempfile::tempdir().expect("temporary ledger directory");
        let (mut ledger, _) = reopen(dir.path(), "of an empty directory").await;
        ledger
            .append(&end_lines(&[&first]))
            .await
            .expect("append the valid prefix");
        append_raw(dir.path(), b"this line is not a ledger record\n");
        let damaged = file_bytes(dir.path());

        match ledger.append(&batch).await {
            Err(JobLedgerError::Corrupt { path, line }) => {
                assert_eq!(path, ledger_path(dir.path()), "{shape}: the ledger file");
                assert_eq!(line, 2, "{shape}: the damaged line");
            }
            other => panic!("{shape}: write over an invalid line must be corrupt: {other:?}"),
        }
        assert_eq!(
            file_bytes(dir.path()),
            damaged,
            "{shape}: a refused write leaves every byte as found"
        );
    }
}

/// Attack: an End for an id that is already durable, with a different
/// outcome or different text, batched behind a fresh End. The write must
/// be refused as a whole with a typed `Corrupt`: neither the conflicting
/// End nor the fresh one reaches the file, and the file still reopens
/// with only the first report.
#[tokio::test]
async fn a_conflicting_same_id_end_is_refused_as_corrupt_and_appends_nothing() {
    let first = report("first", "first report");
    let fresh = report("fresh", "fresh report");
    let conflicts = [
        (
            "a different outcome",
            JobReport {
                outcome: JobOutcome::Exited { code: 1 },
                ..first.clone()
            },
        ),
        (
            "a different text",
            JobReport {
                text: "a conflicting report".into(),
                ..first.clone()
            },
        ),
    ];

    for (shape, conflicting) in conflicts {
        let dir = tempfile::tempdir().expect("temporary ledger directory");
        let (mut ledger, _) = reopen(dir.path(), "of an empty directory").await;
        ledger
            .append(&end_lines(&[&first]))
            .await
            .expect("append the first report");
        let durable = file_bytes(dir.path());

        match ledger.append(&end_lines(&[&fresh, &conflicting])).await {
            Err(JobLedgerError::Corrupt { path, .. }) => {
                assert_eq!(path, ledger_path(dir.path()), "{shape}: the ledger file");
            }
            other => panic!("{shape}: conflicting End must be corrupt: {other:?}"),
        }
        assert_eq!(
            file_bytes(dir.path()),
            durable,
            "{shape}: a refused write appends nothing, not even the fresh End"
        );
        drop(ledger);

        let (_, recovered) = reopen(dir.path(), "after a refused conflicting write").await;
        assert_eq!(recovered.ended, vec![first.clone()], "{shape}");
        assert_eq!(recovered.pending, vec![first.clone()], "{shape}");
    }
}

/// Guard: the refusals above must not break the retry the ledger exists
/// to absorb. An End identical to a durable one is acknowledged and
/// appends no byte, and a retried batch that adds a new report keeps the
/// repeated report on exactly one physical line.
#[tokio::test]
async fn an_identical_end_retry_is_acknowledged_and_keeps_one_physical_copy() {
    let dir = tempfile::tempdir().expect("temporary ledger directory");
    let first = report("first", "first report");
    let second = report("second", "second report");

    let (mut ledger, _) = reopen(dir.path(), "of an empty directory").await;
    ledger
        .append(&end_lines(&[&first]))
        .await
        .expect("append the first report");
    let durable = file_bytes(dir.path());

    ledger
        .append(&end_lines(&[&first]))
        .await
        .expect("an identical retry is acknowledged");
    assert_eq!(
        file_bytes(dir.path()),
        durable,
        "an identical retry appends no byte"
    );
    assert_one_ledger_record(dir.path(), first.id);

    ledger
        .append(&end_lines(&[&first, &second]))
        .await
        .expect("a retried batch with a new report is acknowledged");
    assert_one_ledger_record(dir.path(), first.id);
    assert_one_ledger_record(dir.path(), second.id);
    drop(ledger);

    let (_, recovered) = reopen(dir.path(), "after identical retries").await;
    assert_eq!(recovered.ended, vec![first.clone(), second.clone()]);
    assert_eq!(recovered.pending, vec![first, second]);
}

/// State fixture, not a failed append: a successful write followed by a
/// truncation to the disk state of a batch whose first line landed whole
/// and whose second line landed in part. A second ledger value, opened
/// on that file, writes the same batch again, as a retried flush does
/// with its retained batch; the first value's own bookkeeping, if any,
/// never sees the tear.
#[tokio::test]
async fn state_fixture_batch_rewritten_over_a_torn_partial_append_reopens_each_report_once() {
    let dir = tempfile::tempdir().expect("temporary ledger directory");
    let first = report("first", "first report");
    let second = report("second", "second report");

    let batch = end_lines(&[&first, &second]);
    let (mut ledger, _) = reopen(dir.path(), "of an empty directory").await;
    ledger
        .append(&batch)
        .await
        .expect("the first attempt reaches the disk");
    drop(ledger);
    let bytes = file_bytes(dir.path());
    let second_start = bytes
        .iter()
        .position(|&byte| byte == b'\n')
        .expect("the first line is terminated")
        + 1;
    let cut = second_start + (bytes.len() - second_start) / 2;
    truncate_to(dir.path(), cut);

    let (mut ledger, _) = reopen(dir.path(), "over the partial append").await;
    ledger
        .append(&batch)
        .await
        .expect("the retry appends the whole batch");
    drop(ledger);

    let (_, recovered) = reopen(dir.path(), "after a retried partial append").await;
    assert_eq!(
        recovered.ended,
        vec![first.clone(), second.clone()],
        "each terminal report appears once, in order"
    );
    assert_eq!(recovered.pending, vec![first, second]);
}

/// State fixture, not a failed append: the first line of a batch is
/// written alone to stand for a partial append that landed it whole. A
/// second ledger value, opened on that file, writes the whole batch, as
/// a retried flush does with its retained batch.
#[tokio::test]
async fn state_fixture_batch_rewritten_over_a_durable_first_line_reopens_each_report_once() {
    let dir = tempfile::tempdir().expect("temporary ledger directory");
    let first = report("first", "first report");
    let second = report("second", "second report");

    let (mut ledger, _) = reopen(dir.path(), "of an empty directory").await;
    ledger
        .append(&end_lines(&[&first]))
        .await
        .expect("the first line is durable");
    drop(ledger);

    let (mut ledger, _) = reopen(dir.path(), "over the durable first line").await;
    ledger
        .append(&end_lines(&[&first, &second]))
        .await
        .expect("the retry appends the whole batch");
    drop(ledger);

    let (_, recovered) = reopen(dir.path(), "after a retried partial append").await;
    assert_eq!(
        recovered.ended,
        vec![first.clone(), second.clone()],
        "each terminal report appears once, in order"
    );
    assert_eq!(recovered.pending, vec![first, second]);
}
