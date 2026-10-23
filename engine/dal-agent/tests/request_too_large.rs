//! A provider 413 compacts the history and retries the turn instead of failing it.
//!
//! The same harness drives the compactor chain through a real `Host`, a real
//! `Store` journal and the scripted provider. Scripted compactors size their
//! replacements from the covered entries' own token estimates, the figures
//! the driver subtracts from the projected total, so a "non-shrinking"
//! replacement is non-shrinking at the shared estimator rate whatever the seed
//! holds. Every run also records that accounting, and the tests assert it, so
//! a fixture that drifts from the driver fails on its premise instead of
//! passing for the wrong reason.
//!
//! The cancellation rows pin one promise the chain must keep: once the user
//! has cancelled the turn, the driver stops the chain instead of spending
//! the fallback. A cancelled turn must not start later compactors and must
//! not leak their verdict notices; today's driver only honors a compactor's
//! own typed cancellation, so `a_user_cancel_with_a_chain_moving_primary`
//! fails against production and turns green with the driver guard.
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
use std::cmp::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, CompactError, CompactInput, Compaction, Compactor, ExtensionBuilder, Services,
};
use dal_agent::{Delivery, Env, Host, Product, ServiceError, SessionRef};
use dal_core::{
    CancelScope, ClientId, Command, Config, ConfigProduct, Entry, EntryId, EntryKind, Expect,
    JournalPart, PageReq, Part, Product as JournalProduct, Record, ServiceSet, SessionId, Stop,
    TurnId, UpdateKind, Workspace, estimate_text_tokens,
};
use dal_store::Store;
use tokio::sync::Notify;

/// How long any one step of a scenario may take before the test fails.
const WAIT: Duration = Duration::from_secs(10);

/// The user prompt every overflow scenario submits.
const PROMPT: &str = "please continue";

/// What the shrinking script commits.
const SHRUNK: &str = "shrunk";

/// Tokens the `Larger` script adds beyond what its span removes.
const EXCESS_TOKENS: u64 = 64;

/// The provider's first answer in the overflow scenarios: a 413.
const OVERFLOW: &str = "{\"kind\":\"fail\",\"message\":\"Request Entity Too Large\",\"status\":413,\"family\":\"openai_chat\"}\n";

/// One scripted turn that streams `text`, ends the turn, and reports ten input tokens.
fn events_step(text: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"{text}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

/// One 413 answer, then one complete reply for the retried request. The reply
/// is the only step left, so a request the 413 turn never retried is still
/// available to whatever turn runs next.
fn overflow_script() -> String {
    format!("{OVERFLOW}{}", events_step("recovered"))
}

/// One 413 answer whose failing stream first reports an all-zero usage, then
/// one complete reply for the retried request. The driver records streamed
/// usage before the terminal failure, so the overflow compaction runs with a
/// zero report unless the driver treats an unreported input count as
/// unmeasured and falls back to the leaf estimate.
fn zero_usage_overflow_script() -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":0,\"cached_input_tokens\":0,\"output_tokens\":0,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}}]}}\n{OVERFLOW}{}",
        events_step("recovered")
    )
}

/// One compactor invocation: what it was handed and what it committed.
#[derive(Clone, Debug)]
struct Run {
    /// Name the compactor was registered under.
    name: &'static str,
    /// Sum of the covered entries' own estimates. The driver subtracts the live
    /// part of it and credits an earlier compaction's replacement; no seed here
    /// holds an earlier compaction, so for these runs the whole sum comes off.
    covered_tokens: u64,
    /// Projected token total the driver measures against.
    total_tokens: u64,
    /// Estimate of the replacement it committed, when it committed one.
    replacement_tokens: Option<u64>,
}

impl Run {
    /// The driver's bookkeeping for a committed replacement as
    /// `(tokens_before, tokens_after)`: the covered estimates come off the
    /// projected total and the replacement's estimate goes on.
    fn accounting(&self) -> Option<(u64, u64)> {
        let replacement = self.replacement_tokens?;
        let after = self
            .total_tokens
            .saturating_sub(self.covered_tokens)
            .saturating_add(replacement);
        Some((self.total_tokens, after))
    }
}

/// The shared run log every scripted compactor appends to, in invocation order.
type Runs = Arc<Mutex<Vec<Run>>>;

/// What a scripted compactor yields when it runs.
#[derive(Clone, Copy)]
enum Script {
    /// Commits a replacement that costs exactly what its span removes, so
    /// the context ends the pass exactly as large as it began.
    Equal,
    /// Commits a replacement that costs more than its span removes.
    Larger,
    /// Commits a small replacement that costs far less than its span.
    Shrink,
    /// Refuses with a typed failure.
    Refuse,
    /// Reports the compaction cancelled: the typed cancellation the chain
    /// stops on, never a refusal that moves it on.
    Cancel,
}

impl Script {
    /// The result for one span of which `removed` estimated tokens leave the
    /// context. `removed` is the smaller of the covered estimates and the
    /// projected total, the most the driver's accounting can take off.
    fn result(
        self,
        span: (EntryId, EntryId),
        removed: u64,
    ) -> Result<Option<Compaction>, CompactError> {
        let replacement = match self {
            Self::Equal => text_costing(removed),
            Self::Larger => text_costing(removed + EXCESS_TOKENS),
            Self::Shrink => SHRUNK.to_owned(),
            Self::Refuse => return Err(CompactError::fail("refused")),
            Self::Cancel => return Err(CompactError::Service(ServiceError::Cancelled)),
        };
        Ok(Some(Compaction::text(span, replacement, None)))
    }
}

/// Text whose estimate at the shared 3.5-characters-per-token rate is exactly
/// `tokens`.
fn text_costing(tokens: u64) -> String {
    let characters =
        usize::try_from(tokens.saturating_mul(7) / 2).expect("the cost fits in memory");
    "x".repeat(characters)
}

/// Parks a compactor mid-run so a test can act on a live compaction.
#[derive(Clone, Default)]
struct Hold {
    /// Signalled once the compactor is running and parked.
    entered: Arc<Notify>,
    /// Signalled by the test to let the parked compactor finish.
    release: Arc<Notify>,
}

/// Records its run, then commits or refuses by its script.
struct ScriptedCompactor {
    /// Name this instance reports into the run log.
    name: &'static str,
    /// Shared run log: one entry per compactor invocation.
    runs: Runs,
    /// What this compactor yields.
    script: Script,
    /// Parks the compactor after it logs its run, when set.
    hold: Option<Hold>,
}

impl Compactor for ScriptedCompactor {
    fn compact<'a>(
        &'a self,
        input: CompactInput<'a>,
        _services: Arc<dyn Services>,
    ) -> BoxFuture<'a, Result<Option<Compaction>, CompactError>> {
        let covered_tokens: u64 = input
            .covered
            .iter()
            .map(|entry| entry.estimated_tokens)
            .sum();
        let removed = covered_tokens.min(input.total_tokens);
        let result = self.script.result(input.span, removed);
        let replacement_tokens = if let Ok(Some(compaction)) = &result {
            compaction.summary_text().map(estimate_text_tokens)
        } else {
            None
        };
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Run {
                name: self.name,
                covered_tokens,
                total_tokens: input.total_tokens,
                replacement_tokens,
            });
        let hold = self.hold.clone();
        Box::pin(async move {
            if let Some(hold) = hold {
                hold.entered.notify_one();
                hold.release.notified().await;
            }
            result
        })
    }
}

/// One link of the compactor chain a scenario registers.
#[derive(Clone)]
struct Link {
    /// Name the compactor registers under.
    name: &'static str,
    /// What it yields.
    script: Script,
    /// Whether it registers as a fallback: tried after every primary.
    fallback: bool,
    /// Parks it mid-run, when set.
    hold: Option<Hold>,
}

impl Link {
    /// Parks this link mid-run on `hold`.
    fn held(mut self, hold: &Hold) -> Self {
        self.hold = Some(hold.clone());
        self
    }
}

/// A primary compactor.
fn primary(name: &'static str, script: Script) -> Link {
    Link {
        name,
        script,
        fallback: false,
        hold: None,
    }
}

/// A compactor of last resort.
fn fallback(name: &'static str, script: Script) -> Link {
    Link {
        name,
        script,
        fallback: true,
        hold: None,
    }
}

/// One compactor registration: name, implementation, and whether it is a fallback.
type Registration = (&'static str, Arc<dyn Compactor>, bool);

fn entry(id: u64) -> EntryId {
    EntryId::new(std::num::NonZeroU64::new(id).expect("entry id is non-zero"))
}

fn user(id: u64, parent: Option<EntryId>, text: String) -> Record {
    Record::User(Entry {
        id: entry(id),
        parent,
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

/// Starts a host on `data` whose scripted provider reads `script` from
/// `fixture_name` and whose `chain` extension registers `compactors`.
async fn start_host(
    data: &std::path::Path,
    workspace_dir: &std::path::Path,
    fixture_name: &str,
    script: &str,
    compactors: Vec<Registration>,
) -> Host {
    let fixture = data.join(fixture_name);
    std::fs::write(&fixture, script).expect("write provider fixture");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgon, data, "", Some(&config_text)).expect("config");
    let mut builder =
        ExtensionBuilder::new("chain", "0.1.0", ServiceSet::EMPTY).expect("extension builder");
    for (name, compactor, is_fallback) in compactors {
        builder = if is_fallback {
            builder.fallback_compactor(name, compactor)
        } else {
            builder.compactor(name, compactor)
        };
    }
    let extension = builder.build().expect("chain extension");
    let product = Product {
        name: "dal",
        data_root: data.to_path_buf(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: std::collections::BTreeMap::from([(
            std::ffi::OsString::from("OPENAI_API_KEY"),
            std::ffi::OsString::from("sk-test"),
        )]),
        cwd: workspace_dir.to_path_buf(),
        sandbox_helper: None,
    };
    Host::start(product, config, env).await.expect("host")
}

/// Builds the host over a seeded session with the given compactors.
///
/// The second entry alone outweighs the 20,000-token keep window, so the cut
/// keeps it and the covered span is the first entry. The compactors read that
/// span's weight from `CompactInput::covered` rather than assuming it.
async fn compact_chain_fixture(
    script: &str,
    compactors: Vec<Registration>,
) -> (tempfile::TempDir, dal_agent::Agent, Host, SessionId) {
    let temp = tempfile::tempdir().expect("temp directory");
    let data = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
    let session = SessionId::new_v7();
    let store = Store::new(data.clone(), workspace.clone(), JournalProduct::Dal);
    let mut journal = store.create_session(session);
    journal
        .append(vec![user(1, None, "old prefix ".repeat(2_000))])
        .await
        .expect("seed first user");
    journal
        .append(vec![user(2, Some(entry(1)), "retained".repeat(20_000))])
        .await
        .expect("seed second user");
    journal.close().await.expect("close seeded session");

    let host = start_host(&data, &workspace_dir, "script.jsonl", script, compactors).await;
    let agent = host
        .open(
            SessionRef::Resume {
                workspace,
                key: session.to_string().into(),
            },
            ClientId::new("compactor-chain"),
        )
        .await
        .expect("resume seeded session");
    (temp, agent, host, session)
}

/// The scripted compactors of `chain`, every one logging into `runs`.
fn registrations(chain: &[Link], runs: &Runs) -> Vec<Registration> {
    chain
        .iter()
        .map(|link| {
            let compactor: Arc<dyn Compactor> = Arc::new(ScriptedCompactor {
                name: link.name,
                runs: Arc::clone(runs),
                script: link.script,
                hold: link.hold.clone(),
            });
            (link.name, compactor, link.fallback)
        })
        .collect()
}

/// A started host over the seeded session, with the scripted chain's run log.
struct Rig {
    _temp: tempfile::TempDir,
    agent: dal_agent::Agent,
    host: Host,
    session: SessionId,
    runs: Runs,
}

impl Rig {
    /// Starts the host with `chain` registered in the given order.
    async fn start(script: &str, chain: &[Link]) -> Self {
        let runs: Runs = Arc::default();
        let registrations = registrations(chain, &runs);
        let (temp, agent, host, session) = compact_chain_fixture(script, registrations).await;
        Self {
            _temp: temp,
            agent,
            host,
            session,
            runs,
        }
    }

    /// The compactor invocations so far, in order.
    fn runs(&self) -> Vec<Run> {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The summary text of every compaction entry the journal view holds.
    fn summaries(&self) -> Vec<String> {
        let view = self.agent.view(PageReq::default()).expect("session view");
        view.entries
            .items
            .into_iter()
            .filter_map(|entry| compaction_summary(entry.kind))
            .collect()
    }

    /// How many entries the journal view holds.
    fn entry_count(&self) -> usize {
        self.agent
            .view(PageReq::default())
            .expect("session view")
            .entries
            .items
            .len()
    }

    async fn close(self) {
        self.host.close(self.session).await.expect("close session");
    }
}

/// The summary text of a compaction entry, `None` for any other entry.
fn compaction_summary(kind: EntryKind) -> Option<String> {
    let EntryKind::Compaction { summary, .. } = kind else {
        return None;
    };
    Some(summary.as_deref().unwrap_or_default().to_owned())
}

/// Runs `step`, failing the test when it outlasts `WAIT`.
async fn within<T>(step: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, step)
        .await
        .expect("the step finishes in time")
}

async fn submit_prompt(agent: &dal_agent::Agent, text: &str) {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("submit prompt");
}

/// What one drained turn reported.
struct TurnLog {
    stop: Stop,
    /// Streamed assistant text.
    text: String,
    /// Kind of every notice the turn published, in order.
    notices: Vec<String>,
}

/// The next session update, skipping delivery markers that are not updates.
async fn next_update(subscription: &mut dal_agent::Subscription) -> Arc<dal_core::Update> {
    within(async {
        loop {
            match subscription.next().await {
                Some(Delivery::Update(update)) => return update,
                Some(_) => {}
                None => panic!("subscription closed before the awaited update"),
            }
        }
    })
    .await
}

/// Reads the subscription up to the next turn end.
async fn drain_turn(subscription: &mut dal_agent::Subscription) -> TurnLog {
    let mut text = String::new();
    let mut notices = Vec::new();
    loop {
        let update = next_update(subscription).await;
        match &update.kind {
            UpdateKind::Delta { text: delta, .. } => text.push_str(delta),
            UpdateKind::Notice(notice) => notices.push(notice.kind.to_string()),
            UpdateKind::TurnEnded { stop, .. } => {
                return TurnLog {
                    stop: *stop,
                    text,
                    notices,
                };
            }
            _ => {}
        }
    }
}

/// Reads the subscription up to the next turn start and names that turn.
async fn started_turn(subscription: &mut dal_agent::Subscription) -> TurnId {
    loop {
        let update = next_update(subscription).await;
        if let UpdateKind::TurnStarted { turn, .. } = &update.kind {
            return *turn;
        }
    }
}

/// Reads notice kinds until one equals `kind`, and returns every kind seen,
/// that one last.
async fn notices_until(subscription: &mut dal_agent::Subscription, kind: &str) -> Vec<String> {
    let mut seen = Vec::new();
    loop {
        let update = next_update(subscription).await;
        let UpdateKind::Notice(notice) = &update.kind else {
            continue;
        };
        seen.push(notice.kind.to_string());
        if notice.kind.as_ref() == kind {
            return seen;
        }
    }
}

/// How many of `notices` have this kind.
fn count(notices: &[String], kind: &str) -> usize {
    notices.iter().filter(|seen| seen.as_str() == kind).count()
}

/// The names of the compactors that ran, in order.
fn names(log: &[Run]) -> Vec<&'static str> {
    log.iter().map(|run| run.name).collect()
}

/// Checks the committed replacement of one run against the driver's own
/// accounting: whether the context ends the pass smaller, equal or larger.
fn assert_accounting(run: &Run, expected: Ordering) {
    assert!(
        run.covered_tokens > 0,
        "{} was handed a span with no estimated weight; the seed no longer gives the compaction anything to replace",
        run.name
    );
    let (before, after) = run
        .accounting()
        .expect("the run committed a replacement to account for");
    assert_eq!(
        after.cmp(&before),
        expected,
        "{} accounts {before} to {after} tokens (covered {}, projected total {})",
        run.name,
        run.covered_tokens,
        run.total_tokens
    );
}

/// What one overflow turn left behind.
struct Pass {
    turn: TurnLog,
    runs: Vec<Run>,
    summaries: Vec<String>,
}

/// Drives one prompt through the 413 fixture over `chain`.
async fn run_413(chain: &[Link]) -> Pass {
    run_413_with_script(&overflow_script(), chain).await
}

/// Drives one prompt through a custom provider script over `chain`.
async fn run_413_with_script(script: &str, chain: &[Link]) -> Pass {
    let rig = Rig::start(script, chain).await;
    let mut subscription = rig.agent.subscribe(None).expect("subscribe");
    submit_prompt(&rig.agent, PROMPT).await;
    let turn = drain_turn(&mut subscription).await;
    let pass = Pass {
        turn,
        runs: rig.runs(),
        summaries: rig.summaries(),
    };
    rig.close().await;
    pass
}

/// What a user cancel in the middle of a compaction left behind.
struct CancelPass {
    /// The turn the user cancelled.
    cancelled: TurnLog,
    /// The prompt submitted after the cancelled turn ended.
    followed: TurnLog,
    runs: Vec<Run>,
    summaries: Vec<String>,
}

/// Cancels the 413 turn through the public command while its primary
/// compactor is parked mid-run, lets that compactor finish with `released`,
/// then submits one more prompt.
///
/// The user's cancel is real: it ends the turn in the fold while the driver
/// is still inside the chain. The chain consumes only a compactor's typed
/// cancellation, which `released` supplies or withholds. The cancelled turn
/// publishes its end only after the driver has finished the chain, so the run
/// log is complete by then. The provider script holds one reply after the
/// 413; the follow-up turn receiving it proves the cancelled turn never
/// retried its request.
async fn cancel_during_compaction(released: Script) -> CancelPass {
    let hold = Hold::default();
    let rig = Rig::start(
        &overflow_script(),
        &[
            primary("held", released).held(&hold),
            fallback("fallback", Script::Shrink),
        ],
    )
    .await;
    let mut subscription = rig.agent.subscribe(None).expect("subscribe");
    submit_prompt(&rig.agent, PROMPT).await;
    let turn = started_turn(&mut subscription).await;
    within(hold.entered.notified()).await;
    rig.agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await
        .expect("the cancel is accepted");
    hold.release.notify_one();
    let cancelled = drain_turn(&mut subscription).await;
    submit_prompt(&rig.agent, "follow up").await;
    let followed = drain_turn(&mut subscription).await;
    let pass = CancelPass {
        cancelled,
        followed,
        runs: rig.runs(),
        summaries: rig.summaries(),
    };
    rig.close().await;
    pass
}

/// What one manual compaction left behind.
struct Manual {
    /// Kind of every notice up to and including the awaited one.
    notices: Vec<String>,
    runs: Vec<Run>,
    summaries: Vec<String>,
}

/// Warms the seeded session with one clean turn, runs a manual compaction
/// over `chain`, and waits for a notice of kind `until`.
async fn manual_compaction(chain: &[Link], until: &str) -> Manual {
    let rig = Rig::start(&events_step("warm"), chain).await;
    let mut subscription = rig.agent.subscribe(None).expect("subscribe");
    submit_prompt(&rig.agent, "warm").await;
    let warm = drain_turn(&mut subscription).await;
    assert_eq!(warm.stop, Stop::EndTurn, "the warm turn ends cleanly");
    assert!(
        rig.runs().is_empty(),
        "nothing compacts before the manual compaction"
    );
    rig.agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction is accepted");
    let notices = notices_until(&mut subscription, until).await;
    let manual = Manual {
        notices,
        runs: rig.runs(),
        summaries: rig.summaries(),
    };
    rig.close().await;
    manual
}

#[tokio::test]
async fn http_413_compacts_and_retries_the_turn() {
    let pass = run_413(&[primary("only", Script::Shrink)]).await;

    assert_eq!(
        pass.turn.stop,
        Stop::EndTurn,
        "a 413 must not fail the turn"
    );
    assert_eq!(names(&pass.runs), ["only"], "one compaction pass ran");
    assert_accounting(&pass.runs[0], Ordering::Less);
    assert_eq!(
        pass.summaries,
        [SHRUNK],
        "the shrinking replacement is committed to the journal"
    );
    assert_eq!(
        count(&pass.turn.notices, "compaction_ended"),
        1,
        "one verdict ends the pass"
    );
}

#[tokio::test]
async fn zero_usage_before_overflow_still_compacts_and_retries_the_turn() {
    let pass = run_413_with_script(
        &zero_usage_overflow_script(),
        &[primary("only", Script::Shrink)],
    )
    .await;

    assert_eq!(
        pass.turn.stop,
        Stop::EndTurn,
        "a 413 must not fail the turn"
    );
    assert_eq!(names(&pass.runs), ["only"], "one compaction pass ran");
    assert_accounting(&pass.runs[0], Ordering::Less);
    assert_eq!(
        pass.summaries,
        [SHRUNK],
        "the shrinking replacement is committed even though the failed request streamed an all-zero usage first"
    );
    assert_eq!(
        pass.turn.text, "recovered",
        "the turn retried after the compaction instead of ending on the unmeasured report"
    );
    assert_eq!(
        count(&pass.turn.notices, "compaction_ended"),
        1,
        "one verdict ends the pass"
    );
}

#[tokio::test]
async fn a_nonshrinking_primary_yields_to_the_shrinking_fallback() {
    let pass = run_413(&[
        primary("first", Script::Equal),
        fallback("second", Script::Shrink),
    ])
    .await;

    assert_eq!(
        names(&pass.runs),
        ["first", "second"],
        "the primary runs first, then the fallback"
    );
    assert_accounting(&pass.runs[0], Ordering::Equal);
    assert_accounting(&pass.runs[1], Ordering::Less);
    assert_eq!(
        pass.summaries,
        [SHRUNK],
        "the fallback's replacement is the one committed, and only it"
    );
    assert_eq!(pass.turn.stop, Stop::EndTurn);
    assert_eq!(
        count(&pass.turn.notices, "compaction_ended"),
        1,
        "the rejected candidate does not end the pass; one verdict does"
    );
}

#[tokio::test]
async fn all_nonshrinking_compactors_reject_the_compaction_and_continue_the_turn() {
    let pass = run_413(&[
        primary("first", Script::Larger),
        fallback("second", Script::Equal),
    ])
    .await;

    assert_eq!(
        names(&pass.runs),
        ["first", "second"],
        "every compactor runs before the pass settles"
    );
    assert_accounting(&pass.runs[0], Ordering::Greater);
    assert_accounting(&pass.runs[1], Ordering::Equal);
    assert!(
        pass.summaries.is_empty(),
        "no replacement is committed when none shrinks the context: {:?}",
        pass.summaries
    );
    assert_eq!(
        pass.turn.stop,
        Stop::EndTurn,
        "a rejected compaction continues the turn; it does not fail it"
    );
    assert_eq!(
        count(&pass.turn.notices, "compaction_ended"),
        1,
        "one verdict ends the pass"
    );
}

#[tokio::test]
async fn a_refusing_primary_yields_to_the_shrinking_fallback() {
    let pass = run_413(&[
        primary("first", Script::Refuse),
        fallback("second", Script::Shrink),
    ])
    .await;

    assert_eq!(names(&pass.runs), ["first", "second"]);
    assert_eq!(pass.runs[0].replacement_tokens, None, "the primary refused");
    assert_accounting(&pass.runs[1], Ordering::Less);
    assert_eq!(pass.summaries, [SHRUNK]);
    assert_eq!(pass.turn.stop, Stop::EndTurn);
}

#[tokio::test]
async fn a_refusal_does_not_mask_a_nonshrinking_replacement_in_either_order() {
    for (index, chain) in [
        [
            primary("first", Script::Refuse),
            fallback("second", Script::Equal),
        ],
        [
            primary("first", Script::Equal),
            fallback("second", Script::Refuse),
        ],
    ]
    .into_iter()
    .enumerate()
    {
        let pass = run_413(&chain).await;

        assert_eq!(
            names(&pass.runs),
            ["first", "second"],
            "chain {index}: both compactors run"
        );
        assert_eq!(
            pass.turn.stop,
            Stop::EndTurn,
            "chain {index}: a non-shrinking replacement is a rejected compaction that continues the turn, not the failure of an all-refused chain"
        );
        assert!(
            pass.summaries.is_empty(),
            "chain {index}: no replacement is committed: {:?}",
            pass.summaries
        );
    }
}

#[tokio::test]
async fn an_all_refusing_chain_fails_the_overflow_turn_without_compacting() {
    let pass = run_413(&[
        primary("first", Script::Refuse),
        fallback("second", Script::Refuse),
    ])
    .await;

    assert_eq!(
        names(&pass.runs),
        ["first", "second"],
        "a refusal moves on to the next compactor"
    );
    assert_eq!(
        pass.turn.stop,
        Stop::Failed,
        "overflow recovery fails when every compactor refuses"
    );
    assert!(
        pass.summaries.is_empty(),
        "nothing is committed: {:?}",
        pass.summaries
    );
}

#[tokio::test]
async fn a_compactor_reporting_cancellation_ends_the_chain_without_a_fallback() {
    let pass = run_413(&[
        primary("first", Script::Cancel),
        fallback("second", Script::Shrink),
    ])
    .await;

    assert_eq!(
        names(&pass.runs),
        ["first"],
        "a typed cancellation stops the chain; the fallback never runs"
    );
    assert_eq!(
        pass.turn.stop,
        Stop::Failed,
        "overflow recovery fails instead of trying the fallback"
    );
    assert!(
        pass.summaries.is_empty(),
        "nothing is committed: {:?}",
        pass.summaries
    );
}

#[tokio::test]
async fn a_typed_cancellation_after_a_user_cancel_ends_the_chain_before_any_fallback() {
    let pass = cancel_during_compaction(Script::Cancel).await;

    assert_eq!(
        pass.cancelled.stop,
        Stop::Cancelled,
        "the user's cancel ends the turn"
    );
    assert_eq!(
        names(&pass.runs),
        ["held"],
        "a typed cancellation ends the chain; the fallback never runs"
    );
    assert!(
        pass.summaries.is_empty(),
        "a cancelled turn commits no compaction: {:?}",
        pass.summaries
    );
    assert_eq!(pass.followed.stop, Stop::EndTurn);
    assert_eq!(
        pass.followed.text, "recovered",
        "the cancelled turn issued no retry: the next turn still received the scripted reply"
    );
}

#[tokio::test]
async fn a_replacement_finishing_after_a_user_cancel_is_never_committed() {
    let pass = cancel_during_compaction(Script::Shrink).await;

    assert_eq!(pass.cancelled.stop, Stop::Cancelled);
    assert_eq!(names(&pass.runs), ["held"]);
    assert_accounting(&pass.runs[0], Ordering::Less);
    assert!(
        pass.summaries.is_empty(),
        "a replacement that would have been accepted is dropped once the turn was cancelled: {:?}",
        pass.summaries
    );
    assert_eq!(pass.followed.stop, Stop::EndTurn);
    assert_eq!(
        pass.followed.text, "recovered",
        "the late replacement did not resume the cancelled turn's retry"
    );
}

#[tokio::test]
async fn a_user_cancel_with_a_chain_moving_primary_stops_before_any_fallback() {
    let pass = cancel_during_compaction(Script::Equal).await;

    assert_eq!(
        pass.cancelled.stop,
        Stop::Cancelled,
        "the user's cancel ends the turn"
    );
    assert_eq!(
        names(&pass.runs),
        ["held"],
        "a cancelled turn must not start the fallback: the chain stops once the turn is cancelled, whatever the parked primary yields"
    );
    assert_eq!(
        count(&pass.cancelled.notices, "compaction_not_shrunk"),
        0,
        "a cancelled turn leaks no chain verdict notices: {:?}",
        pass.cancelled.notices
    );
    assert!(
        pass.summaries.is_empty(),
        "the equal replacement must not be committed: {:?}",
        pass.summaries
    );
    assert_eq!(pass.followed.stop, Stop::EndTurn);
    assert_eq!(
        pass.followed.text, "recovered",
        "the cancelled turn issued no retry: the next turn still received the scripted reply"
    );
}

#[tokio::test]
async fn manual_compaction_falls_through_to_the_shrinking_fallback() {
    let manual = manual_compaction(
        &[
            primary("first", Script::Equal),
            fallback("second", Script::Shrink),
        ],
        "compaction_ended",
    )
    .await;

    assert_eq!(
        names(&manual.runs),
        ["first", "second"],
        "the primary runs first, then the fallback"
    );
    assert_accounting(&manual.runs[0], Ordering::Equal);
    assert_accounting(&manual.runs[1], Ordering::Less);
    assert_eq!(
        manual.summaries,
        [SHRUNK],
        "the fallback's replacement is the one committed"
    );
}

#[tokio::test]
async fn manual_compaction_with_only_nonshrinking_compactors_changes_nothing() {
    let manual = manual_compaction(
        &[
            primary("first", Script::Larger),
            fallback("second", Script::Equal),
        ],
        "compact.manual_failed",
    )
    .await;

    assert_eq!(
        names(&manual.runs),
        ["first", "second"],
        "every compactor runs before the manual compaction fails"
    );
    assert_accounting(&manual.runs[0], Ordering::Greater);
    assert_accounting(&manual.runs[1], Ordering::Equal);
    assert!(
        manual.summaries.is_empty(),
        "no replacement is committed: {:?}",
        manual.summaries
    );
    assert_eq!(
        count(&manual.notices, "compaction_ended"),
        1,
        "one verdict precedes the manual failure"
    );
}

/// A host that goes down while a compaction is running leaves no durable
/// trace of it: the restarted host replays the old projection, no compactor
/// runs by itself, and the next compaction commits normally.
#[tokio::test]
async fn a_restart_during_a_compaction_replays_the_old_projection() {
    let hold = Hold::default();
    let rig = Rig::start(
        &events_step("warm"),
        &[primary("held", Script::Shrink).held(&hold)],
    )
    .await;
    let mut subscription = rig.agent.subscribe(None).expect("subscribe");
    submit_prompt(&rig.agent, "warm").await;
    let warm = drain_turn(&mut subscription).await;
    assert_eq!(warm.stop, Stop::EndTurn, "the warm turn ends cleanly");
    let before = rig.entry_count();
    rig.agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction is accepted");
    within(hold.entered.notified()).await;
    assert_eq!(names(&rig.runs()), ["held"], "the compaction is running");

    // The host goes down with the compactor parked mid-run: the shutdown
    // cancels the compaction, nothing was appended, and the session lock is
    // released for the next process.
    let Rig {
        _temp: temp,
        agent,
        host,
        session,
        ..
    } = rig;
    within(host.close(session))
        .await
        .expect("a host closes while a compaction runs");
    drop(subscription);
    drop(agent);
    drop(host);

    let data = temp.path().join("data");
    let workspace_dir = temp.path().join("workspace");
    let runs: Runs = Arc::default();
    let restarted = start_host(
        &data,
        &workspace_dir,
        "script-restart.jsonl",
        &events_step("after the restart"),
        registrations(&[primary("fresh", Script::Shrink)], &runs),
    )
    .await;
    let agent = restarted
        .open(
            SessionRef::Resume {
                workspace: Workspace::new(workspace_dir).expect("workspace"),
                key: session.to_string().into(),
            },
            ClientId::new("compactor-chain-restart"),
        )
        .await
        .expect("resume the interrupted session");
    let view = agent.view(PageReq::default()).expect("session view");
    assert_eq!(
        view.entries.items.len(),
        before,
        "the replayed branch is the one before the interrupted compaction"
    );
    assert!(
        !view
            .entries
            .items
            .iter()
            .any(|entry| matches!(entry.kind, EntryKind::Compaction { .. })),
        "no phantom compaction entry survives the restart"
    );
    assert!(
        runs.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty(),
        "the restart runs no compactor by itself"
    );

    let mut subscription = agent.subscribe(None).expect("subscribe");
    submit_prompt(&agent, "after the restart").await;
    let turn = drain_turn(&mut subscription).await;
    assert_eq!(turn.stop, Stop::EndTurn);
    assert_eq!(
        turn.text, "after the restart",
        "the restarted session answers from the scripted provider's first reply"
    );
    agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction is accepted");
    let notices = notices_until(&mut subscription, "compaction_ended").await;
    assert_eq!(count(&notices, "compaction_ended"), 1);
    let view = agent.view(PageReq::default()).expect("session view");
    let summaries: Vec<String> = view
        .entries
        .items
        .into_iter()
        .filter_map(|entry| compaction_summary(entry.kind))
        .collect();
    assert_eq!(
        summaries,
        [SHRUNK],
        "the restarted session compacts normally: nothing was left wedged"
    );
    restarted.close(session).await.expect("close session");
}
