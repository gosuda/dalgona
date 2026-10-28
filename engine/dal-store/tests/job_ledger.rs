//! The `Store`-owned job ledger through its public door only.
//!
//! The unit tests beside the ledger drive its private open on a directory that already
//! exists. These tests start at `Store::new` and the public `Store::job_ledger`, locate every
//! path through `Store::session_jobs_dir`, and then look at the real filesystem: what a
//! read-only open leaves alone, what the first append creates, and what a failed or refused
//! call leaves behind for its retry.
//!
//! Unix permission bits are asserted only under `cfg(unix)`, where the production helpers set
//! and check mode bits. The Windows helpers act on ACLs, which these fixtures cannot build
//! portably, so no Windows permission result is claimed here: a Windows host runs only the
//! platform-neutral checks (existence, bytes, directory listings, `file_is_private`).
//!
//! No test touches the process umask, the environment, or a lint waiver. Fixtures return
//! `Result`, and every absence check goes through `try_exists`, so a permission error while
//! looking is an error and never a silent "absent".

use std::{
    collections::HashSet,
    error::Error,
    ffi::OsString,
    fmt::Debug,
    fs, io,
    path::{Path, PathBuf},
};

use dal_core::{JobId, JobOutcome, JobReport, Product, SessionId, Workspace};
use dal_store::{
    JobLedger, JobLedgerEntry, JobLedgerError, JobLedgerRecovery, Store, file_is_private,
};

/// The durable name of the ledger file inside a session jobs directory. Older builds wrote
/// this exact name, so renaming it would orphan every existing ledger.
const LEDGER_FILE: &str = "reports.jsonl";

#[cfg(unix)]
const UNENFORCED: &str = "chmod is not enforced for this user (root or an ACL bypass), so the permission contract cannot be exercised; run the permission tests unprivileged";

type TestResult = Result<(), Box<dyn Error>>;

/// A `Store` over a fresh temporary tree plus the one session id whose ledger a test uses.
struct Fixture {
    /// Keeps the temporary tree alive; the tree is removed on drop.
    _temp: tempfile::TempDir,
    /// The absolute data root handed to `Store::new`. The fixture never creates it.
    data_root: PathBuf,
    workspace: Workspace,
    store: Store,
    session: SessionId,
}

impl Fixture {
    /// Builds a store whose absolute data root is the temporary directory plus `parts`.
    fn new(parts: &[&str]) -> Result<Self, Box<dyn Error>> {
        let temp = tempfile::tempdir()?;
        let mut data_root = temp.path().to_path_buf();
        for part in parts {
            data_root.push(part);
        }
        if !data_root.is_absolute() {
            return Err(format!("data root {} is not absolute", data_root.display()).into());
        }
        let workspace = Workspace::new(temp.path().join("workspace"))?;
        let store = Store::new(data_root.clone(), workspace.clone(), Product::Dalgona);
        Ok(Self {
            _temp: temp,
            data_root,
            workspace,
            store,
            session: SessionId::new_v7(),
        })
    }

    /// The jobs directory, as the public `Store` derives it.
    fn jobs_dir(&self) -> PathBuf {
        self.store.session_jobs_dir(self.session)
    }

    fn ledger_file(&self) -> PathBuf {
        self.jobs_dir().join(LEDGER_FILE)
    }

    /// The jobs directory and each ancestor up to and including the data root, leaf first.
    fn chain(&self) -> io::Result<Vec<PathBuf>> {
        let jobs_dir = self.jobs_dir();
        let mut dirs = Vec::new();
        for ancestor in jobs_dir.ancestors() {
            dirs.push(ancestor.to_path_buf());
            if ancestor == self.data_root.as_path() {
                return Ok(dirs);
            }
        }
        Err(io::Error::other(format!(
            "jobs directory {} is not under the data root {}",
            jobs_dir.display(),
            self.data_root.display()
        )))
    }

    /// A brand-new `Store` over the same data root and workspace: what a restarted process has.
    fn restarted_store(&self) -> Store {
        Store::new(
            self.data_root.clone(),
            self.workspace.clone(),
            Product::Dalgona,
        )
    }

    async fn open(
        &self,
        delivered: &HashSet<JobId>,
    ) -> Result<(JobLedger, JobLedgerRecovery), JobLedgerError> {
        self.store.job_ledger(self.session, delivered).await
    }

    async fn reopen(
        &self,
        delivered: &HashSet<JobId>,
    ) -> Result<(JobLedger, JobLedgerRecovery), JobLedgerError> {
        let restarted = self.restarted_store();
        restarted.job_ledger(self.session, delivered).await
    }
}

fn report(label: &str, outcome: JobOutcome, text: &str) -> JobReport {
    JobReport {
        id: JobId::new_v7(),
        label: label.into(),
        outcome,
        text: text.into(),
    }
}

/// Four reports with distinct ids, every outcome shape, and every field filled with a value
/// no other report shares, so a swapped or dropped field changes the comparison.
fn known_reports() -> [JobReport; 4] {
    [
        report(
            "compile \"core\" \\ release",
            JobOutcome::Exited { code: 101 },
            "first line\nsecond line \u{2014} \u{fc}n\u{ef} \u{2713}",
        ),
        report(
            "deploy",
            JobOutcome::Failed {
                message: "disk full".into(),
            },
            "no space left on device",
        ),
        report("lint", JobOutcome::Cancelled, "cancelled by the user"),
        report("orphan", JobOutcome::Lost, "outcome unknown after restart"),
    ]
}

fn entries(reports: &[JobReport]) -> Vec<JobLedgerEntry> {
    reports.iter().map(JobLedger::end).collect()
}

/// The ledger file's bytes for `reports`: one newline-terminated line per report, encoded as
/// the entry's documented transparent encoding, which is the file's line format.
fn encoded_lines(reports: &[JobReport]) -> io::Result<Vec<u8>> {
    let mut text = String::new();
    for found in reports {
        let line = sonic_rs::to_string(&JobLedger::end(found))
            .map_err(|error| io::Error::other(error.to_string()))?;
        text.push_str(&line);
        text.push('\n');
    }
    Ok(text.into_bytes())
}

fn assert_empty(recovered: &JobLedgerRecovery, when: &str) {
    assert!(
        recovered.ended.is_empty() && recovered.pending.is_empty() && recovered.held.is_empty(),
        "{when}: an absent or empty ledger replays as empty, got {recovered:?}"
    );
}

/// The typed `Corrupt` an operation returned: the ledger file and the damaged line.
fn expect_corrupt<T: Debug>(
    result: Result<T, JobLedgerError>,
) -> Result<(PathBuf, usize), Box<dyn Error>> {
    match result {
        Err(JobLedgerError::Corrupt { path, line }) => Ok((path, line)),
        Err(other) => Err(format!("expected Corrupt, got {other:?}").into()),
        Ok(value) => Err(format!("expected Corrupt, but the call succeeded: {value:?}").into()),
    }
}

/// The typed `Io` an operation returned: the failing path and the operating system error.
fn expect_io<T: Debug>(
    result: Result<T, JobLedgerError>,
) -> Result<(PathBuf, io::Error), Box<dyn Error>> {
    match result {
        Err(JobLedgerError::Io { path, source }) => Ok((path, source)),
        Err(other) => Err(format!("expected Io, got {other:?}").into()),
        Ok(value) => Err(format!("expected Io, but the call succeeded: {value:?}").into()),
    }
}

/// The sorted entry names of `dir`.
fn names(dir: &Path) -> io::Result<Vec<OsString>> {
    let mut found = fs::read_dir(dir)?
        .map(|entry| entry.map(|inner| inner.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    found.sort();
    Ok(found)
}

/// The file's bytes, or `None` only when the file is missing. Any other failure, a permission
/// error included, is an error and never reads as "absent".
fn read_if_present(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Asserts a freshly built chain holds nothing but itself: every directory lists exactly its
/// child toward the leaf, so a stray temporary or sibling file would fail here.
fn assert_tree_is_only_the_chain(dirs: &[PathBuf]) -> io::Result<()> {
    for pair in dirs.windows(2) {
        let [child, parent] = pair else {
            return Err(io::Error::other("windows(2) yields pairs"));
        };
        let expected: Vec<OsString> = child.file_name().map(OsString::from).into_iter().collect();
        assert_eq!(
            names(parent)?,
            expected,
            "{} lists only the next directory down",
            parent.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn mode(path: &Path) -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(unix)]
fn set_mode(path: &Path, bits: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(bits))
}

/// The permission bits of every directory and, when it exists, the file.
#[cfg(unix)]
fn modes_of(dirs: &[PathBuf], file: &Path, file_present: bool) -> io::Result<Vec<(PathBuf, u32)>> {
    let mut found = Vec::new();
    for dir in dirs {
        found.push((dir.clone(), mode(dir)?));
    }
    if file_present {
        found.push((file.to_path_buf(), mode(file)?));
    }
    Ok(found)
}

/// Everything a read-only open or a refused call must leave alone.
#[derive(Debug, PartialEq)]
struct Snapshot {
    /// The ledger file's bytes, `None` while the file does not exist.
    bytes: Option<Vec<u8>>,
    /// Each directory of the chain with its sorted entry names.
    listings: Vec<(PathBuf, Vec<OsString>)>,
    /// Unix permission bits of every directory and the file; empty on other hosts, where this
    /// file builds no permission fixture.
    modes: Vec<(PathBuf, u32)>,
}

fn snapshot(dirs: &[PathBuf], file: &Path) -> io::Result<Snapshot> {
    let bytes = read_if_present(file)?;
    let mut listings = Vec::with_capacity(dirs.len());
    for dir in dirs {
        listings.push((dir.clone(), names(dir)?));
    }
    #[cfg(unix)]
    let modes = modes_of(dirs, file, bytes.is_some())?;
    #[cfg(not(unix))]
    let modes = Vec::new();
    Ok(Snapshot {
        bytes,
        listings,
        modes,
    })
}

/// Creates the session jobs tree by hand, the way an older build left it: ordinary
/// directories, with every directory from the jobs directory up to the data root at 0755 on
/// Unix. Returns the chain, leaf first. The ledger file is the caller's to plant.
fn legacy_tree(fixture: &Fixture) -> io::Result<Vec<PathBuf>> {
    let dirs = fixture.chain()?;
    fs::create_dir_all(fixture.jobs_dir())?;
    #[cfg(unix)]
    for dir in &dirs {
        set_mode(dir, 0o755)?;
    }
    Ok(dirs)
}

/// Attack: the data root does not exist at all. `Store::new` and the read-only open must create
/// nothing, an empty read must replay as empty, and the first appends must build the whole
/// tree privately. Two whole reports per batch, with every outcome shape, must come back from a
/// restarted store with every field intact, ended and pending alike.
#[tokio::test]
async fn absent_root_reads_empty_then_append_builds_a_private_tree() -> TestResult {
    let fixture = Fixture::new(&["data"])?;
    assert!(
        !fixture.data_root.try_exists()?,
        "Store::new must not create the data root"
    );

    let (mut ledger, recovered) = fixture.open(&HashSet::new()).await?;
    assert_empty(&recovered, "first open of an absent tree");
    assert!(
        !fixture.data_root.try_exists()?,
        "a read-only open must not create the data root"
    );

    let reports = known_reports();
    ledger.append(&entries(&reports[..2])).await?;
    ledger.append(&entries(&reports[2..])).await?;
    drop(ledger);

    let dirs = fixture.chain()?;
    let jobs_dir = fixture.jobs_dir();
    let ledger_file = fixture.ledger_file();
    assert_tree_is_only_the_chain(&dirs)?;
    assert_eq!(
        names(&jobs_dir)?,
        vec![OsString::from(LEDGER_FILE)],
        "the jobs directory holds exactly the ledger file"
    );
    for dir in &dirs {
        assert!(
            fs::symlink_metadata(dir)?.is_dir(),
            "{} is a real directory",
            dir.display()
        );
        #[cfg(unix)]
        assert_eq!(mode(dir)?, 0o700, "{} is owner-only", dir.display());
    }
    assert!(
        fs::symlink_metadata(&ledger_file)?.is_file(),
        "the ledger is a regular file"
    );
    assert!(
        file_is_private(&fs::File::open(&ledger_file)?)?,
        "the new ledger file is private"
    );
    #[cfg(unix)]
    assert_eq!(mode(&ledger_file)?, 0o600, "the new ledger file is 0600");
    assert_eq!(
        fs::read(&ledger_file)?,
        encoded_lines(&reports)?,
        "one encoded end line per report, in append order"
    );

    let restarted = fixture.restarted_store();
    assert_eq!(
        restarted.session_jobs_dir(fixture.session),
        jobs_dir,
        "a restarted store derives the same ledger location"
    );
    let (_, recovered) = restarted
        .job_ledger(fixture.session, &HashSet::new())
        .await?;
    assert_eq!(
        recovered.ended,
        reports.to_vec(),
        "every ended report, whole"
    );
    assert_eq!(
        recovered.pending,
        reports.to_vec(),
        "with nothing delivered, every report is pending"
    );
    assert!(recovered.held.is_empty(), "no job was held");

    let delivered = HashSet::from([reports[1].id, reports[3].id]);
    let (_, recovered) = restarted.job_ledger(fixture.session, &delivered).await?;
    assert_eq!(
        recovered.ended,
        reports.to_vec(),
        "delivery never shrinks ended"
    );
    assert_eq!(
        recovered.pending,
        vec![reports[0].clone(), reports[2].clone()],
        "delivered ids leave pending, in order"
    );
    Ok(())
}

/// Attack: a ledger tree an older build left with shared permissions: file 0644, every
/// directory 0755. Appending to it and reading it must neither tighten nor loosen any mode and
/// must keep the old bytes, not just the metadata. Unix only: the Windows private helpers act on
/// DACLs, no portable fixture builds a legacy ACL, and the Windows result is unobserved here.
#[cfg(unix)]
#[tokio::test]
async fn legacy_shared_modes_survive_append_and_both_reports_recover() -> TestResult {
    let fixture = Fixture::new(&["data"])?;
    let dirs = legacy_tree(&fixture)?;
    let ledger_file = fixture.ledger_file();
    let legacy = report(
        "legacy",
        JobOutcome::Exited { code: 0 },
        "written by an older build",
    );
    let fresh = report("fresh", JobOutcome::Cancelled, "written after the upgrade");
    fs::write(&ledger_file, encoded_lines(std::slice::from_ref(&legacy))?)?;
    set_mode(&ledger_file, 0o644)?;
    for dir in &dirs {
        assert_eq!(
            mode(dir)?,
            0o755,
            "{} starts as a legacy directory",
            dir.display()
        );
    }
    assert_eq!(mode(&ledger_file)?, 0o644, "the legacy file starts shared");
    assert!(
        !file_is_private(&fs::File::open(&ledger_file)?)?,
        "negative control: the legacy fixture really is shared"
    );
    let planted = snapshot(&dirs, &ledger_file)?;

    let (mut ledger, recovered) = fixture.open(&HashSet::new()).await?;
    assert_eq!(
        recovered.ended,
        vec![legacy.clone()],
        "the legacy report recovers"
    );
    assert_eq!(
        snapshot(&dirs, &ledger_file)?,
        planted,
        "a read-only open migrates nothing"
    );

    ledger
        .append(&entries(std::slice::from_ref(&fresh)))
        .await?;
    let appended = snapshot(&dirs, &ledger_file)?;
    assert_eq!(
        appended.bytes,
        Some(encoded_lines(&[legacy.clone(), fresh.clone()])?),
        "the legacy bytes stay and the new line follows them"
    );
    assert_eq!(
        appended.modes, planted.modes,
        "append neither tightens nor loosens any mode"
    );
    assert_eq!(
        appended.listings, planted.listings,
        "append adds no sibling file"
    );
    assert!(
        !file_is_private(&fs::File::open(&ledger_file)?)?,
        "the legacy file is still shared after the append"
    );

    let (_, reopened) = fixture.reopen(&HashSet::new()).await?;
    assert_eq!(
        reopened.ended,
        vec![legacy.clone(), fresh.clone()],
        "both distinct reports recover, oldest first"
    );
    assert_eq!(reopened.pending, vec![legacy, fresh]);
    Ok(())
}

/// Attack: a legacy-permission ledger whose third line is complete (newline-terminated) but
/// damaged, followed by a valid line. Two damage shapes the unit tests do not use: valid JSON
/// with an unknown record tag, and a terminated line of invalid UTF-8. The public open must
/// return the typed `Corrupt` naming this ledger file and line 3, on both of two opens, and
/// neither open may repair bytes, add or remove a sibling, or change a mode. Every damaged line
/// is terminated: a torn unterminated tail belongs to the unit tests beside the ledger.
#[tokio::test]
async fn corrupt_legacy_ledger_is_typed_and_leaves_bytes_and_modes_alone() -> TestResult {
    let damage = [
        (
            "an unknown record tag",
            b"{\"e\":\"vanished\"}\n".as_slice(),
        ),
        ("invalid UTF-8", b"\xff\xfe\n".as_slice()),
    ];
    for (shape, damaged_line) in damage {
        let fixture = Fixture::new(&["data"])?;
        let dirs = legacy_tree(&fixture)?;
        let ledger_file = fixture.ledger_file();
        let reports = known_reports();
        let mut bytes = encoded_lines(&reports[..2])?;
        bytes.extend_from_slice(damaged_line);
        bytes.extend_from_slice(&encoded_lines(&reports[2..3])?);
        fs::write(&ledger_file, bytes)?;
        #[cfg(unix)]
        {
            set_mode(&ledger_file, 0o644)?;
            assert_eq!(mode(&ledger_file)?, 0o644, "{shape}: the legacy file mode");
        }
        let planted = snapshot(&dirs, &ledger_file)?;

        for attempt in ["first", "second"] {
            let (path, line) = expect_corrupt(fixture.open(&HashSet::new()).await)?;
            assert_eq!(
                path, ledger_file,
                "{shape}, {attempt} open: the ledger file"
            );
            assert_eq!(line, 3, "{shape}, {attempt} open: the damaged line");
            assert_eq!(
                snapshot(&dirs, &ledger_file)?,
                planted,
                "{shape}, {attempt} open: bytes, listings and modes are untouched"
            );
        }
    }
    Ok(())
}

/// Attack: the session jobs directory exists (a legacy 0755 tree on Unix) but holds no ledger
/// file. The open must replay empty and create no file, and an empty batch must not create one
/// either. Only a real batch creates the ledger, as a private file, without touching the
/// existing directory modes.
#[tokio::test]
async fn existing_dir_without_ledger_reads_empty_then_adds_a_private_file() -> TestResult {
    let fixture = Fixture::new(&["data"])?;
    let dirs = legacy_tree(&fixture)?;
    let jobs_dir = fixture.jobs_dir();
    let ledger_file = fixture.ledger_file();
    let planted = snapshot(&dirs, &ledger_file)?;
    assert_eq!(
        planted.bytes, None,
        "fixture: the ledger file does not exist yet"
    );

    let (mut ledger, recovered) = fixture.open(&HashSet::new()).await?;
    assert_empty(&recovered, "open of a jobs directory without a ledger");
    assert!(
        !ledger_file.try_exists()?,
        "a read-only open creates no ledger file"
    );
    assert_eq!(
        snapshot(&dirs, &ledger_file)?,
        planted,
        "the open changed nothing"
    );
    ledger.append(&[]).await?;
    assert!(
        !ledger_file.try_exists()?,
        "an empty batch creates no ledger file"
    );
    assert_eq!(
        snapshot(&dirs, &ledger_file)?,
        planted,
        "the empty batch changed nothing"
    );

    let reports = known_reports();
    ledger.append(&entries(&reports[..1])).await?;
    assert_eq!(
        fs::read(&ledger_file)?,
        encoded_lines(&reports[..1])?,
        "the first real batch is the whole file"
    );
    assert_eq!(
        names(&jobs_dir)?,
        vec![OsString::from(LEDGER_FILE)],
        "the jobs directory gained only the ledger file"
    );
    assert!(
        file_is_private(&fs::File::open(&ledger_file)?)?,
        "a ledger file created in a legacy directory is private"
    );
    #[cfg(unix)]
    {
        assert_eq!(mode(&ledger_file)?, 0o600, "the new ledger file is 0600");
        for dir in &dirs {
            assert_eq!(mode(dir)?, 0o755, "{} keeps its legacy mode", dir.display());
        }
    }
    let (_, reopened) = fixture.reopen(&HashSet::new()).await?;
    assert_eq!(reopened.ended, reports[..1].to_vec());
    Ok(())
}

/// Attack: the ledger file exists and is zero bytes long. That is a complete, empty ledger, not
/// corruption and not a torn tail: the open replays empty and changes nothing, and the first
/// append must write exactly its own line into that file, keeping its existing mode.
#[tokio::test]
async fn existing_empty_ledger_reads_empty_and_first_append_writes_only_its_line() -> TestResult {
    let fixture = Fixture::new(&["data"])?;
    let dirs = legacy_tree(&fixture)?;
    let ledger_file = fixture.ledger_file();
    fs::File::create(&ledger_file)?;
    #[cfg(unix)]
    set_mode(&ledger_file, 0o644)?;
    let planted = snapshot(&dirs, &ledger_file)?;
    assert_eq!(
        planted.bytes,
        Some(Vec::new()),
        "fixture: a zero-length ledger file"
    );

    let (mut ledger, recovered) = fixture.open(&HashSet::new()).await?;
    assert_empty(&recovered, "open of an empty ledger file");
    assert_eq!(
        snapshot(&dirs, &ledger_file)?,
        planted,
        "the open changed nothing"
    );

    let reports = known_reports();
    ledger.append(&entries(&reports[..1])).await?;
    let appended = snapshot(&dirs, &ledger_file)?;
    assert_eq!(
        appended.bytes,
        Some(encoded_lines(&reports[..1])?),
        "the file is exactly the one appended line"
    );
    assert_eq!(
        appended.modes, planted.modes,
        "the existing file keeps its mode"
    );
    assert_eq!(
        appended.listings, planted.listings,
        "append adds no sibling file"
    );
    let (_, reopened) = fixture.reopen(&HashSet::new()).await?;
    assert_eq!(reopened.ended, reports[..1].to_vec());
    Ok(())
}

/// Attack: the first append cannot build the tree because the data root's own parent is missing,
/// so the durable create refuses before making anything. The failure must be a typed `Io` that
/// names the missing directory with `NotFound`, must leave nothing behind, and must not poison the
/// handle: after the parent appears, the same handle appends the same batch once, into a fully
/// private new tree.
#[tokio::test]
async fn failed_tree_creation_leaves_nothing_and_the_retry_lands_once() -> TestResult {
    let fixture = Fixture::new(&["absent", "data"])?;
    let missing_parent = fixture
        .data_root
        .parent()
        .ok_or("the data root has no parent")?
        .to_path_buf();
    let (mut ledger, recovered) = fixture.open(&HashSet::new()).await?;
    assert_empty(&recovered, "open under a missing parent");

    let reports = known_reports();
    let batch = entries(&reports[..1]);
    let (path, source) = expect_io(ledger.append(&batch).await)?;
    assert_eq!(
        path, missing_parent,
        "the error names the directory that is missing"
    );
    assert_eq!(
        source.kind(),
        io::ErrorKind::NotFound,
        "the operating system error"
    );
    assert!(
        !missing_parent.try_exists()?,
        "the refused append created nothing, not even the missing parent"
    );

    fs::create_dir(&missing_parent)?;
    ledger.append(&batch).await?;
    drop(ledger);
    let dirs = fixture.chain()?;
    assert_tree_is_only_the_chain(&dirs)?;
    #[cfg(unix)]
    for dir in &dirs {
        assert_eq!(mode(dir)?, 0o700, "{} is owner-only", dir.display());
    }
    assert_eq!(
        fs::read(fixture.ledger_file())?,
        encoded_lines(&reports[..1])?,
        "the retried batch is on disk exactly once"
    );
    let (_, reopened) = fixture.reopen(&HashSet::new()).await?;
    assert_eq!(reopened.ended, reports[..1].to_vec());
    Ok(())
}

/// Attack: the data root exists but is not writable. A read-only open must still work, the first
/// append must be refused with a typed `Io` carrying `PermissionDenied` and create nothing, and
/// once the mode is restored the same handle must succeed: a failed tree creation never latches
/// the handle as having established its directories. Unix only, and, as in the lifecycle
/// permission tests, it fails loudly when the user bypasses chmod (root).
#[cfg(unix)]
#[tokio::test]
async fn unwritable_root_refuses_first_append_and_the_retry_after_chmod_lands() -> TestResult {
    let fixture = Fixture::new(&["data"])?;
    fs::create_dir(&fixture.data_root)?;
    set_mode(&fixture.data_root, 0o500)?;
    if fs::File::create(fixture.data_root.join("probe")).is_ok() {
        set_mode(&fixture.data_root, 0o700)?;
        fs::remove_file(fixture.data_root.join("probe"))?;
        return Err(UNENFORCED.into());
    }

    let (mut ledger, recovered) = fixture.open(&HashSet::new()).await?;
    assert_empty(&recovered, "open under a read-only data root");
    let reports = known_reports();
    let batch = entries(&reports[..1]);
    let refused = ledger.append(&batch).await;
    set_mode(&fixture.data_root, 0o700)?;
    let (path, source) = expect_io(refused)?;
    assert!(
        path.starts_with(&fixture.data_root),
        "the error names a path under the data root, got {}",
        path.display()
    );
    assert_eq!(
        source.kind(),
        io::ErrorKind::PermissionDenied,
        "the operating system error"
    );
    assert!(
        names(&fixture.data_root)?.is_empty(),
        "the refused append created nothing under the data root"
    );

    ledger.append(&batch).await?;
    drop(ledger);
    let dirs = fixture.chain()?;
    assert_tree_is_only_the_chain(&dirs)?;
    for dir in &dirs {
        assert_eq!(mode(dir)?, 0o700, "{} is owner-only", dir.display());
    }
    assert_eq!(
        fs::read(fixture.ledger_file())?,
        encoded_lines(&reports[..1])?,
        "the retried batch is on disk exactly once"
    );
    let (_, reopened) = fixture.reopen(&HashSet::new()).await?;
    assert_eq!(reopened.ended, reports[..1].to_vec());
    Ok(())
}

/// Attack: the data root exists but cannot be searched, so looking for the ledger fails with a
/// permission error. The open must report that as a typed `Io` naming the ledger file, not
/// swallow it as an absent ledger and replay empty: an empty replay here would let the next
/// append start a second history beside the unreadable one. `try_exists` is the probe this
/// file's absence checks rely on, and it must report the same error instead of `false`. Once
/// the root is searchable again the open replays empty and nothing was created. Unix only, and
/// it fails loudly when the user bypasses chmod (root).
#[cfg(unix)]
#[tokio::test]
async fn unsearchable_root_opens_as_typed_io_not_as_an_empty_ledger() -> TestResult {
    let fixture = Fixture::new(&["data"])?;
    fs::create_dir(&fixture.data_root)?;
    set_mode(&fixture.data_root, 0o000)?;
    let probe = fixture.data_root.join("sessions").try_exists();
    let opened = fixture.open(&HashSet::new()).await;
    set_mode(&fixture.data_root, 0o700)?;
    let denied = matches!(&probe, Err(error) if error.kind() == io::ErrorKind::PermissionDenied);
    assert!(
        denied,
        "{UNENFORCED}; try_exists on an unsearchable root returned {probe:?}"
    );

    let (path, source) = expect_io(opened)?;
    assert_eq!(
        path,
        fixture.ledger_file(),
        "the error names the ledger file"
    );
    assert_eq!(
        source.kind(),
        io::ErrorKind::PermissionDenied,
        "the operating system error"
    );
    let (_, recovered) = fixture.open(&HashSet::new()).await?;
    assert_empty(&recovered, "open once the root is searchable again");
    assert!(
        names(&fixture.data_root)?.is_empty(),
        "neither open created anything under the data root"
    );
    Ok(())
}

/// Attack: an empty batch after a read-only open. It is acknowledged with no I/O: the absent
/// data root must stay absent. A later non-empty batch must really land, which only a read of the
/// disk and a restarted store prove, not the returned `Ok`. An empty batch after the ledger
/// exists must leave its bytes alone.
#[tokio::test]
async fn empty_batch_does_no_io_and_the_later_batch_lands_whole() -> TestResult {
    let fixture = Fixture::new(&["data"])?;
    let (mut ledger, recovered) = fixture.open(&HashSet::new()).await?;
    assert_empty(&recovered, "first open of an absent tree");

    ledger.append(&[]).await?;
    assert!(
        !fixture.data_root.try_exists()?,
        "an empty batch is acknowledged without creating the data root"
    );

    let reports = known_reports();
    ledger.append(&entries(&reports[..1])).await?;
    let ledger_file = fixture.ledger_file();
    assert!(
        ledger_file.try_exists()?,
        "the non-empty batch created the ledger"
    );
    let landed = fs::read(&ledger_file)?;
    assert_eq!(
        landed,
        encoded_lines(&reports[..1])?,
        "the whole payload is on disk"
    );

    ledger.append(&[]).await?;
    assert_eq!(
        fs::read(&ledger_file)?,
        landed,
        "an empty batch leaves an existing ledger byte for byte alone"
    );
    drop(ledger);

    let (_, reopened) = fixture.reopen(&HashSet::new()).await?;
    assert_eq!(
        reopened.ended,
        reports[..1].to_vec(),
        "a restarted store recovers the whole report"
    );
    Ok(())
}
