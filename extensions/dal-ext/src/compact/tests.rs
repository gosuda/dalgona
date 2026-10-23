//! The compaction chain end to end: the real `compact` extension on a real
//! `Host`, a real `Store` journal and the scripted provider.
//!
//! The scripted provider serves every request in order, so the script is the
//! trace of who asked: a compactor that runs consumes the next step, and the
//! text that step carries reaches the journal or the notices. No test here
//! mocks a model or a compactor.
//!
//! Two facts about the host decide the scripts. The `remote` compactor sends
//! an ordinary request and refuses any answer that carries no native
//! compaction event, so a scripted host always refuses it with a typed error
//! and the chain moves on to `summary`; each compaction therefore consumes
//! one refused `remote` step before its `summary` step. And the fold measures
//! the context from the provider's reported prompt tokens only while the
//! answering route equals the active one; a session whose requested model is
//! the configured `provider/model` reference never matches at a turn start,
//! so a turn start falls back to the byte estimate while a tool round boundary
//! reads the measured count. The tests below drive both paths.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use dal_agent::ext::Extension;
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, EntryId, EntryKind, Expect, PageReq, Part, SessionId,
    Stop, UpdateKind, Workspace,
};
use tempfile::TempDir;

use super::extension;

/// How long one turn may take before the test fails.
const WAIT: Duration = Duration::from_secs(60);

/// The text of the refused `remote` attempt in every scripted chain.
const REMOTE_REFUSED: &str = "remote step: the scripted route has no native compaction";

/// A summary far smaller than the history it replaces.
const SHRINKING_SUMMARY: &str =
    "## Goal\ncheckpoint of the earlier conversation\n\n## Next steps\n1. continue\n";

/// Prompt bytes of each of the first two turns of an estimate scenario that
/// crosses the threshold on its third turn: 214,000 estimated tokens each,
/// 93 percent of the 461,000-token threshold between them.
const HEAVY_BYTES: usize = 750_000;

/// The same turns in the scenario that stays under it: 171,000 tokens each.
const UNDER_HEAVY_BYTES: usize = 600_000;

/// The third prompt of both scenarios: 57,000 estimated tokens, past the
/// 20,000-token keep window, so the cut lands on it. It lifts the first
/// scenario to 105 percent of the threshold and the second to 87 percent,
/// and what survives a compaction of the first is far under the threshold.
const THIRD_BYTES: usize = 200_000;

/// The tool call every measured scenario makes. The host has no such tool, so
/// the call fails and the round still completes, which opens a round boundary.
const TOOL_NAME: &str = "probe";

/// The first prompt token count that reaches 85 percent of the 922,000-token
/// window of the scripted model.
const MEASURED_AT_THRESHOLD: u64 = 783_700;

fn usage_event(input_tokens: u64) -> String {
    format!(
        "{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":{input_tokens},\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}}"
    )
}

/// One scripted reply. An empty `text` streams no text at all.
fn reply(text: &str, input_tokens: u64) -> String {
    let text_event = if text.is_empty() {
        String::new()
    } else {
        let escaped = sonic_rs::to_string(text).expect("fixture text is valid JSON");
        format!("{{\"type\":\"text_delta\",\"text\":{escaped}}},")
    };
    format!(
        "{{\"kind\":\"events\",\"events\":[{text_event}{{\"type\":\"tool_calls_done\",\"calls\":[]}},{},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n",
        usage_event(input_tokens)
    )
}

/// One scripted reply that calls a tool the host does not have.
fn tool_call(input_tokens: u64) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"tool_call_started\",\"id\":\"c1\",\"name\":\"{TOOL_NAME}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[{{\"id\":\"c1\",\"name\":\"{TOOL_NAME}\",\"args\":{{\"kind\":\"parsed\",\"value\":{{}}}}}}]}},{},{{\"type\":\"stop\",\"reason\":\"tool_use\"}}]}}\n",
        usage_event(input_tokens)
    )
}

fn script(steps: &[String]) -> String {
    steps.concat()
}

/// A data root and a workspace that outlive every host a test starts on them.
struct Site {
    dir: TempDir,
    workspace: Workspace,
}

impl Site {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp directory");
        std::fs::create_dir_all(dir.path().join("data")).expect("data directory");
        let workspace_dir = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
        let workspace = Workspace::new(workspace_dir).expect("workspace");
        Self { dir, workspace }
    }

    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    /// Starts a host whose scripted provider serves `steps` in order.
    ///
    /// `compact_ratio` is the flat config threshold fraction written to the
    /// config; `None` keeps the default of 0.85.
    async fn host(
        &self,
        name: &str,
        steps: &[String],
        compact_ratio: Option<f64>,
        extensions: Vec<Extension>,
    ) -> Host {
        let data = self.data();
        let fixture = data.join(format!("{name}.jsonl"));
        std::fs::write(&fixture, script(steps)).expect("write provider fixture");
        let ratio =
            compact_ratio.map_or_else(String::new, |ratio| format!("compact_ratio = {ratio}\n"));
        let config_text = format!(
            "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n{ratio}\n[providers.scripted]\nfixture = {:?}\n",
            fixture.display().to_string()
        );
        let config =
            Config::load(ConfigProduct::Dalgon, &data, "", Some(&config_text)).expect("config");
        let product = Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions,
            bundled: Vec::new(),
        };
        let env = Env {
            vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
            cwd: self.dir.path().join("workspace"),
            sandbox_helper: None,
        };
        Host::start(product, config, env)
            .await
            .expect("host starts")
    }

    async fn open_new(&self, host: &Host, client: &str) -> (Agent, Subscription) {
        let agent = host
            .open(
                SessionRef::New {
                    workspace: self.workspace.clone(),
                    name: None,
                },
                ClientId::new(client),
            )
            .await
            .expect("open session");
        let subscription = agent.subscribe(None).expect("subscribe");
        (agent, subscription)
    }

    async fn resume(&self, host: &Host, session: SessionId, client: &str) -> (Agent, Subscription) {
        let agent = host
            .open(
                SessionRef::Resume {
                    workspace: self.workspace.clone(),
                    key: session.to_string().into(),
                },
                ClientId::new(client),
            )
            .await
            .expect("resume session");
        let subscription = agent.subscribe(None).expect("subscribe");
        (agent, subscription)
    }
}

/// What one prompt turn published.
struct Turn {
    stop: Stop,
    /// The assistant text streamed during the turn.
    text: String,
    /// `(kind, text)` of every notice, in order.
    notices: Vec<(String, String)>,
    /// The kind of every update, for failure messages.
    trace: Vec<String>,
    /// The ids of the user entries the turn added to the branch.
    users: Vec<EntryId>,
}

impl Turn {
    fn kinds(&self) -> Vec<&str> {
        self.notices.iter().map(|(kind, _)| kind.as_str()).collect()
    }

    fn count(&self, kind: &str) -> usize {
        self.notices.iter().filter(|(seen, _)| seen == kind).count()
    }

    /// The text of the first notice of `kind`, with the turn's notices in the
    /// failure message when there is none.
    fn notice(&self, kind: &str) -> &str {
        self.notices
            .iter()
            .find(|(seen, _)| seen == kind)
            .map_or_else(
                || {
                    panic!(
                        "no `{kind}` notice; notices: {:?}; trace: {:?}",
                        self.notices, self.trace
                    )
                },
                |(_, text)| text.as_str(),
            )
    }

    fn assert_no_compaction(&self) {
        let compaction: Vec<_> = self
            .notices
            .iter()
            .filter(|(kind, _)| kind.starts_with("compact"))
            .collect();
        assert!(
            compaction.is_empty(),
            "no compaction may run: {compaction:?}; trace: {:?}",
            self.trace
        );
    }
}

/// Submits `prompt` and reads the subscription to the end of that turn.
async fn prompt(agent: &Agent, subscription: &mut Subscription, prompt: &str) -> Turn {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: prompt.into(),
            }],
        })
        .await
        .expect("submit prompt");
    let mut turn = Turn {
        stop: Stop::EndTurn,
        text: String::new(),
        notices: Vec::new(),
        trace: Vec::new(),
        users: Vec::new(),
    };
    tokio::time::timeout(WAIT, async {
        loop {
            let Some(Delivery::Update(update)) = subscription.next().await else {
                continue;
            };
            let name = format!("{:?}", update.kind);
            turn.trace
                .push(name.split([' ', '(']).next().unwrap_or_default().to_owned());
            match &update.kind {
                UpdateKind::Delta { text, .. } => turn.text.push_str(text),
                UpdateKind::Tree(delta) => turn.users.extend(
                    delta
                        .added
                        .iter()
                        .filter(|entry| matches!(entry.kind, EntryKind::User { .. }))
                        .map(|entry| entry.id),
                ),
                UpdateKind::Notice(notice) => turn
                    .notices
                    .push((notice.kind.to_string(), notice.text.to_string())),
                UpdateKind::TurnEnded { stop, .. } => {
                    turn.stop = *stop;
                    return;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends in time");
    turn
}

/// Reads notices after a manual `Command::Compact` until the compaction
/// ends, committed or rejected.
async fn manual_compaction(agent: &Agent, subscription: &mut Subscription) -> String {
    agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction accepted");
    tokio::time::timeout(WAIT, async {
        loop {
            let Some(Delivery::Update(update)) = subscription.next().await else {
                continue;
            };
            let UpdateKind::Notice(notice) = &update.kind else {
                continue;
            };
            if matches!(
                notice.kind.as_ref(),
                "compaction_ended" | "compact.manual_failed"
            ) {
                return notice.text.to_string();
            }
        }
    })
    .await
    .expect("the manual compaction ends in time")
}

/// One compaction entry of the journal view.
#[derive(Debug, PartialEq)]
struct Committed {
    summary: Option<String>,
    first_kept: Option<EntryId>,
}

fn compactions(agent: &Agent) -> Vec<Committed> {
    agent
        .view(PageReq::default())
        .expect("session view")
        .entries
        .items
        .into_iter()
        .filter_map(|entry| match entry.kind {
            EntryKind::Compaction {
                summary,
                first_kept,
                ..
            } => Some(Committed {
                summary: summary.map(|text| text.to_string()),
                first_kept,
            }),
            _ => None,
        })
        .collect()
}

fn session_id(agent: &Agent) -> SessionId {
    agent
        .view(PageReq::default())
        .expect("session view")
        .session
        .id
}

fn big(len: usize) -> String {
    "x".repeat(len)
}

fn compact_extensions() -> Vec<Extension> {
    vec![extension().expect("compact extension registers")]
}

/// Runs two heavy turns, then a third whose prompt is `third_bytes` long, and
/// returns the host, the session and the third turn.
///
/// Every reply reports zero prompt tokens, so nothing is measured and each
/// turn start weighs the context by the byte estimate alone. At `ratio` 0.5
/// the threshold is 461,000 tokens, about 1.6 million bytes. The first two
/// turns keep the context under it; the third prompt decides whether the
/// third turn starts over it. The prompt of a turn is already on the branch
/// when its start is weighed.
///
/// `after` is the script after the two heavy replies.
async fn three_turns_over_the_estimate(
    site: &Site,
    heavy_bytes: usize,
    third_bytes: usize,
    after: &[String],
    ratio: f64,
) -> (Host, Agent, Subscription, Turn) {
    let mut steps = vec![reply("turn 1 reply", 0), reply("turn 2 reply", 0)];
    steps.extend_from_slice(after);
    let host = site
        .host("estimate", &steps, Some(ratio), compact_extensions())
        .await;
    let (agent, mut subscription) = site.open_new(&host, "compact-estimate").await;
    let first = prompt(&agent, &mut subscription, &big(heavy_bytes)).await;
    assert_eq!(first.stop, Stop::EndTurn, "turn 1: {:?}", first.trace);
    first.assert_no_compaction();
    let second = prompt(&agent, &mut subscription, &big(heavy_bytes)).await;
    assert_eq!(second.stop, Stop::EndTurn, "turn 2: {:?}", second.trace);
    second.assert_no_compaction();
    let third = prompt(&agent, &mut subscription, &big(third_bytes)).await;
    (host, agent, subscription, third)
}

#[test]
fn registers_remote_first_and_summary_as_the_fallback() {
    let ext = extension().expect("extension registers");
    assert_eq!(ext.name(), "compact");
    let names: Vec<&str> = ext
        .compactors()
        .iter()
        .map(|(name, _)| name.as_ref())
        .collect();
    assert_eq!(
        names,
        vec!["remote", "summary"],
        "registered compactor order must be remote (primary) then summary (fallback)"
    );
    assert!(
        !ext.is_fallback_compactor("remote"),
        "remote must be registered as a primary compactor"
    );
    assert!(
        ext.is_fallback_compactor("summary"),
        "summary must be registered as a fallback compactor"
    );
    let invalid = dal_agent::ext::ExtensionBuilder::new(
        "invalid name with spaces",
        "0.1.0",
        dal_core::ServiceSet::default(),
    );
    assert!(
        invalid.is_err(),
        "an invalid extension name must return RegistrationError, not panic"
    );
}

/// The estimate fallback: nothing was measured, the third turn opens over a
/// context past the threshold, and the chain runs `remote` (refused) then
/// `summary` (committed) before the turn's own request. The retained turn is
/// the one in flight; everything before it is replaced.
#[tokio::test]
async fn estimate_fallback_compacts_through_remote_then_summary_at_a_turn_start() {
    let site = Site::new();
    let (host, agent, _subscription, third) = three_turns_over_the_estimate(
        &site,
        HEAVY_BYTES,
        THIRD_BYTES,
        &[
            reply(REMOTE_REFUSED, 0),
            reply(SHRINKING_SUMMARY, 0),
            reply("turn 3 reply after compaction", 0),
        ],
        0.5,
    )
    .await;

    assert_eq!(third.stop, Stop::EndTurn, "turn 3: {:?}", third.trace);
    assert_eq!(
        third.kinds(),
        ["compaction_started", "compaction_ended"],
        "one compaction opens the turn; trace {:?}",
        third.trace
    );
    assert_eq!(
        third.count("compaction_not_shrunk"),
        0,
        "the summary shrank the context"
    );
    assert!(
        third
            .notice("compaction_ended")
            .starts_with("Context compacted by summary:"),
        "the fallback compactor `summary` committed after `remote` refused: {}",
        third.notice("compaction_ended")
    );
    assert_eq!(
        third.text, "turn 3 reply after compaction",
        "the turn's own request ran after the compaction, not before or inside it"
    );

    assert_eq!(
        compactions(&agent),
        vec![Committed {
            summary: Some(SHRINKING_SUMMARY.to_owned()),
            first_kept: Some(third.users[0]),
        }],
        "one committed replacement holds the `summary` text and keeps the \
         recent user turn: the cut lands on the third prompt"
    );
    host.close(session_id(&agent)).await.expect("close session");
}

/// The same history one notch under the threshold runs no compaction at all.
#[tokio::test]
async fn below_the_estimate_threshold_no_compaction_runs() {
    let site = Site::new();
    let (host, agent, _subscription, third) = three_turns_over_the_estimate(
        &site,
        UNDER_HEAVY_BYTES,
        THIRD_BYTES,
        &[reply("turn 3 reply", 0)],
        0.5,
    )
    .await;

    assert_eq!(third.stop, Stop::EndTurn, "turn 3: {:?}", third.trace);
    third.assert_no_compaction();
    assert_eq!(
        third.text, "turn 3 reply",
        "the turn consumed its own scripted reply, so no compactor took a step"
    );
    assert!(
        compactions(&agent).is_empty(),
        "no compaction entry below the threshold"
    );
    host.close(session_id(&agent)).await.expect("close session");
}

/// One tool round whose answer reports `measured` prompt tokens, then the
/// scripted `after` steps, on the default 85 percent threshold. The prefix
/// turns are light, so a summary of any real size would not shrink them;
/// callers pick the summary accordingly.
async fn measured_round(
    site: &Site,
    measured: u64,
    after: &[String],
    extensions: Vec<Extension>,
) -> (Host, Agent, Subscription, Turn) {
    let mut steps = vec![
        reply("turn 1 reply", 50),
        reply("turn 2 reply", 50),
        tool_call(measured),
    ];
    steps.extend_from_slice(after);
    let host = site.host("measured", &steps, None, extensions).await;
    let (agent, mut subscription) = site.open_new(&host, "compact-measured").await;
    for (index, text) in ["turn 1 prompt", "turn 2 prompt"].into_iter().enumerate() {
        let turn = prompt(&agent, &mut subscription, text).await;
        assert_eq!(turn.stop, Stop::EndTurn, "prefix turn {index}");
        turn.assert_no_compaction();
    }
    let third = prompt(&agent, &mut subscription, &big(70_000)).await;
    (host, agent, subscription, third)
}

/// The measured path: a tool round reported exactly 85 percent of the
/// window, so the next round boundary compacts through the chain.
#[tokio::test]
async fn measured_usage_at_the_threshold_compacts_after_the_tool_round() {
    let site = Site::new();
    // The prefix is light: only a summary of a few tokens shrinks it.
    let (host, agent, _subscription, third) = measured_round(
        &site,
        MEASURED_AT_THRESHOLD,
        &[
            reply(REMOTE_REFUSED, 0),
            reply("short checkpoint", 0),
            reply("turn 3 final reply", 0),
        ],
        compact_extensions(),
    )
    .await;

    assert_eq!(third.stop, Stop::EndTurn, "turn 3: {:?}", third.trace);
    let started = third.notice("compaction_started");
    assert!(
        started.contains("85 percent of the window"),
        "the trigger is the measured fraction of the window: {started}"
    );
    assert!(
        third
            .notice("compaction_ended")
            .starts_with("Context compacted by summary:"),
        "remote refused and summary committed: {:?}",
        third.notices
    );
    assert_eq!(third.text, "turn 3 final reply");
    assert_eq!(compactions(&agent).len(), 1);
    host.close(session_id(&agent)).await.expect("close session");
}

/// One prompt token under the threshold, the same round compacts nothing.
#[tokio::test]
async fn measured_usage_one_token_under_the_threshold_runs_no_compaction() {
    let site = Site::new();
    let (host, agent, _subscription, third) = measured_round(
        &site,
        MEASURED_AT_THRESHOLD - 1,
        &[reply("turn 3 final reply", 0)],
        compact_extensions(),
    )
    .await;

    assert_eq!(third.stop, Stop::EndTurn, "turn 3: {:?}", third.trace);
    third.assert_no_compaction();
    assert_eq!(
        third.text, "turn 3 final reply",
        "the round after the tool call took the next scripted reply, so no compactor ran"
    );
    assert_eq!(compactions(&agent), Vec::new());
    host.close(session_id(&agent)).await.expect("close session");
}

/// A summary that does not shrink the context is rejected: the old context
/// stays, nothing is journaled, and the session keeps working.
#[tokio::test]
async fn a_summary_that_does_not_shrink_the_context_is_rejected_and_the_session_continues() {
    let site = Site::new();
    // The prefix is two tiny turns; this summary costs thousands of tokens.
    let heavy_summary = format!("## Goal\n{}\n", "long non-shrinking summary ".repeat(600));
    let (host, agent, mut subscription, third) = measured_round(
        &site,
        800_000,
        &[
            reply(REMOTE_REFUSED, 0),
            reply(&heavy_summary, 0),
            reply("turn 3 final reply", 0),
            reply("turn 4 reply", 0),
        ],
        compact_extensions(),
    )
    .await;

    assert_eq!(
        third.stop,
        Stop::EndTurn,
        "a rejected compaction must not fail the turn: {:?}",
        third.trace
    );
    assert!(
        third
            .notice("compaction_not_shrunk")
            .starts_with("Compactor summary did not shrink the context ("),
        "the chain names the compactor that did not shrink: {:?}",
        third.notices
    );
    let ended = third.notice("compaction_ended");
    assert!(
        ended.starts_with("Compaction rejected: the summary did not shrink the context (")
            && ended.ends_with(" tokens). The old context is kept."),
        "the core rejects the replacement with its exact text: {ended}"
    );
    assert_eq!(
        third.text, "turn 3 final reply",
        "the turn continued on its own request after the rejection"
    );
    assert_eq!(
        compactions(&agent),
        Vec::new(),
        "a rejected summary leaves no compaction entry"
    );

    let fourth = prompt(&agent, &mut subscription, "turn 4 prompt").await;
    assert_eq!(fourth.stop, Stop::EndTurn, "turn 4: {:?}", fourth.trace);
    assert_eq!(fourth.text, "turn 4 reply", "the session is still usable");
    host.close(session_id(&agent)).await.expect("close session");
}

/// Both compactors refuse: the old context stays and the turn goes on.
#[tokio::test]
async fn when_every_compactor_refuses_the_old_context_is_kept_and_the_turn_continues() {
    let site = Site::new();
    // The summary step carries no text, which the `summary` compactor refuses.
    let (host, agent, _subscription, third) = measured_round(
        &site,
        800_000,
        &[
            reply(REMOTE_REFUSED, 0),
            reply("", 0),
            reply("turn 3 final reply", 0),
        ],
        compact_extensions(),
    )
    .await;

    assert_eq!(third.stop, Stop::EndTurn, "turn 3: {:?}", third.trace);
    assert_eq!(
        third.notice("compaction_ended"),
        "Compaction failed: the model returned no summary text.",
        "the last refusal is the reported error"
    );
    assert_eq!(third.text, "turn 3 final reply");
    assert_eq!(compactions(&agent), Vec::new());
    host.close(session_id(&agent)).await.expect("close session");
}

/// Without any registered compactor the threshold is still noticed, once,
/// and the turn is not blocked.
#[tokio::test]
async fn without_a_compactor_the_threshold_reports_no_compactor_and_inference_continues() {
    let site = Site::new();
    let (host, agent, _subscription, third) = measured_round(
        &site,
        800_000,
        &[reply("turn 3 final reply", 0)],
        Vec::new(),
    )
    .await;

    assert_eq!(third.stop, Stop::EndTurn, "turn 3: {:?}", third.trace);
    assert_eq!(
        third.count("compaction_none"),
        1,
        "the missing compactor is reported once: {:?}",
        third.notices
    );
    assert_eq!(
        third.notice("compaction_none"),
        "No compactor is registered, so auto-compaction does nothing in this session."
    );
    assert_eq!(third.count("compaction_started"), 0);
    assert_eq!(third.text, "turn 3 final reply");
    assert_eq!(compactions(&agent), Vec::new());
    host.close(session_id(&agent)).await.expect("close session");
}

/// Restart after the commit: the reopened session holds the same compaction
/// and the same retained context, and nothing asks the model to compact again.
#[tokio::test]
async fn a_committed_compaction_survives_a_restart_without_another_inference() {
    let site = Site::new();
    let (host, agent, _subscription, third) = three_turns_over_the_estimate(
        &site,
        HEAVY_BYTES,
        THIRD_BYTES,
        &[
            reply(REMOTE_REFUSED, 0),
            reply(SHRINKING_SUMMARY, 0),
            reply("turn 3 reply after compaction", 0),
        ],
        0.5,
    )
    .await;
    assert_eq!(third.stop, Stop::EndTurn, "turn 3: {:?}", third.trace);
    let session = session_id(&agent);
    let before = compactions(&agent);
    assert_eq!(before.len(), 1, "the first run committed one compaction");
    host.close(session).await.expect("close session");
    drop(agent);
    drop(host);

    // The second host's script holds two replies, one per turn. A restart
    // that compacted again would feed a reply to `remote`, and a turn would
    // end with the wrong text or an exhausted script.
    let restarted = site
        .host(
            "restart",
            &[
                reply("turn 4 reply after restart", 0),
                reply("turn 5 reply", 0),
            ],
            Some(0.5),
            compact_extensions(),
        )
        .await;
    let (agent, mut subscription) = site.resume(&restarted, session, "compact-restart").await;
    assert_eq!(
        compactions(&agent),
        before,
        "replay yields the committed compaction, unchanged"
    );
    let fourth = prompt(&agent, &mut subscription, "turn 4 prompt").await;
    assert_eq!(fourth.stop, Stop::EndTurn, "turn 4: {:?}", fourth.trace);
    fourth.assert_no_compaction();
    assert_eq!(
        fourth.text, "turn 4 reply after restart",
        "the first request after the restart is the turn's own"
    );
    // A resumed session knows no context window until its first request, so
    // the turn above could not compact whatever the replayed projection held.
    // The next turn start weighs the replayed branch: it stays under the
    // threshold only if replay projected the retained turn and the summary,
    // not the history they replaced.
    let fifth = prompt(&agent, &mut subscription, "turn 5 prompt").await;
    assert_eq!(fifth.stop, Stop::EndTurn, "turn 5: {:?}", fifth.trace);
    fifth.assert_no_compaction();
    assert_eq!(fifth.text, "turn 5 reply");
    assert_eq!(
        compactions(&agent),
        before,
        "the restart added no second compaction"
    );
    restarted.close(session).await.expect("close session");
}

/// `/compact` through the ordinary command path: `remote` refuses, `summary`
/// commits one text replacement, and a restart replays the same compaction
/// without another inference.
#[tokio::test]
async fn manual_compact_falls_back_to_summary_and_reopens() {
    let site = Site::new();
    let host = site
        .host(
            "manual",
            &[
                reply("turn 1 reply", 0),
                reply("turn 2 reply", 0),
                reply(REMOTE_REFUSED, 0),
                reply(SHRINKING_SUMMARY, 0),
            ],
            None,
            compact_extensions(),
        )
        .await;
    let (agent, mut subscription) = site.open_new(&host, "compact-manual").await;
    let mut kept = None;
    for text in [big(100_000), big(100_000)] {
        let turn = prompt(&agent, &mut subscription, &text).await;
        assert_eq!(turn.stop, Stop::EndTurn, "{:?}", turn.trace);
        turn.assert_no_compaction();
        kept = turn.users.first().copied();
    }

    let ended = manual_compaction(&agent, &mut subscription).await;
    assert!(
        ended.starts_with("Context compacted by summary:"),
        "`remote` refused and `summary` committed: {ended}"
    );
    let committed = compactions(&agent);
    assert_eq!(
        committed,
        vec![Committed {
            summary: Some(SHRINKING_SUMMARY.to_owned()),
            first_kept: kept,
        }],
        "one text-only replacement that keeps the second turn"
    );
    let session = session_id(&agent);
    host.close(session).await.expect("close session");
    drop(agent);
    drop(host);

    let restarted = site
        .host(
            "manual-restart",
            &[reply("turn 3 reply after restart", 0)],
            None,
            compact_extensions(),
        )
        .await;
    let (agent, mut subscription) = site.resume(&restarted, session, "compact-manual-2").await;
    assert_eq!(
        compactions(&agent),
        committed,
        "replay yields the same compaction"
    );
    let third = prompt(&agent, &mut subscription, "turn 3 prompt").await;
    assert_eq!(third.stop, Stop::EndTurn, "{:?}", third.trace);
    third.assert_no_compaction();
    assert_eq!(
        third.text, "turn 3 reply after restart",
        "no inference was repeated for the committed compaction"
    );
    restarted.close(session).await.expect("close session");
}
