#![expect(clippy::expect_used, reason = "integration tests fail loudly")]
#![expect(clippy::too_many_lines, reason = "integration tests fail loudly")]
#![expect(
    clippy::field_reassign_with_default,
    reason = "integration tests fail loudly"
)]
//! Seeded state-model conformance for session lifecycles.

mod support;

use std::{collections::HashMap, num::NonZeroU64};

use dal_core::{
    AssistantStop, Block, CallId, Entry, EntryId, EntryKind, Family, JournalPart, Product, RawJson,
    Record, SessionId, TurnEndStop, TurnId, Usage, Workspace,
};
use dal_store::Store;
use support::temp_dir::TempDir;

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero test entry id"))
}

fn turn_id(value: u64) -> TurnId {
    TurnId::new(NonZeroU64::new(value).expect("nonzero test turn id"))
}

fn timestamp() -> jiff::Timestamp {
    jiff::Timestamp::now()
}

fn usage() -> Usage {
    Usage {
        input_tokens: 1,
        cached_input_tokens: 0,
        output_tokens: 1,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

/// A deterministic command generator; no external randomness is needed.
struct Seeded(u64);

impl Seeded {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_795_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % bound
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Model {
    entries: Vec<EntryId>,
    leaf: Option<EntryId>,
    labels: HashMap<EntryId, Option<String>>,
    archived: bool,
    open_turn: Option<TurnId>,
    next_entry: u64,
    next_turn: u64,
    turn_input: u64,
    turn_output: u64,
}

fn fold(records: &[Record]) -> Model {
    let mut model = Model::default();
    model.next_entry = 1;
    model.next_turn = 1;
    for record in records {
        match record {
            Record::Assistant(entry) => {
                model.entries.push(entry.id);
                model.leaf = Some(entry.id);
                model.next_entry = model.next_entry.max(entry.id.get() + 1);
                if let EntryKind::Assistant { usage, .. } = &entry.kind {
                    model.turn_input += usage.input_tokens;
                    model.turn_output += usage.output_tokens;
                }
            }
            Record::User(entry)
            | Record::ToolResult(entry)
            | Record::Reminder(entry)
            | Record::Model(entry)
            | Record::Thinking(entry)
            | Record::Approval(entry)
            | Record::Mode(entry)
            | Record::Compaction(entry)
            | Record::BranchSummary(entry) => {
                model.entries.push(entry.id);
                model.leaf = Some(entry.id);
                model.next_entry = model.next_entry.max(entry.id.get() + 1);
            }
            Record::Leaf { to, .. } => model.leaf = *to,
            Record::Label { entry, label, .. } => {
                model
                    .labels
                    .insert(*entry, label.as_deref().map(str::to_owned));
            }
            Record::Archive { archived, .. } => model.archived = *archived,
            Record::TurnStart { turn, .. } => {
                model.open_turn = Some(*turn);
                model.next_turn = model.next_turn.max(turn.get() + 1);
                model.turn_input = 0;
                model.turn_output = 0;
            }
            Record::TurnEnd { .. } => model.open_turn = None,
            _ => {}
        }
    }
    model
}

fn user(id: u64) -> Record {
    Record::User(Entry {
        id: entry_id(id),
        parent: None,
        at: timestamp(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: format!("command {id}").into(),
            }],
        },
    })
}

fn assistant_pair(id: u64) -> Vec<Record> {
    let input = RawJson::parse("{}").expect("empty object parses");
    vec![
        Record::Assistant(Entry {
            id: entry_id(id),
            parent: None,
            at: timestamp(),
            kind: EntryKind::Assistant {
                api: Family::Chat,
                model: "test-model".into(),
                content: vec![Block::ToolCall {
                    id: CallId::new(format!("call-{id}")),
                    name: "read".into(),
                    input,
                }],
                usage: usage(),
                stop: AssistantStop::ToolUse,
            },
        }),
        Record::ToolResult(Entry {
            id: entry_id(id + 1),
            parent: None,
            at: timestamp(),
            kind: EntryKind::ToolResult {
                call: CallId::new(format!("call-{id}")),
                name: "read".into(),
                error: false,
                parts: vec![JournalPart::Text { text: "ok".into() }],
                changes: vec![],
                elapsed_ms: None,
            },
        }),
    ]
}

#[tokio::test]
async fn model_commands_match_fold_after_every_command() {
    let temp = TempDir::new("store-model");
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let store = Store::new(temp.path().join("data"), workspace, Product::Dalgona);
    let id = SessionId::new_v7();
    let mut journal = store.create_session(id);
    let mut model = Model::default();
    model.next_entry = 1;
    model.next_turn = 1;
    let mut rng = Seeded(0x12_34_56_78);
    let mut appended = 0usize;
    let mut reopens = 0usize;
    for step in 0..10_000 {
        if model.entries.is_empty() {
            let record = user(model.next_entry);
            journal.append(vec![record]).await.expect("command appends");
            model.entries.push(entry_id(model.next_entry));
            model.leaf = Some(entry_id(model.next_entry));
            model.next_entry += 1;
            appended += 1;
            continue;
        }
        match rng.next(6) {
            0 | 1 => {
                let record = user(model.next_entry);
                journal.append(vec![record]).await.expect("command appends");
                model.entries.push(entry_id(model.next_entry));
                model.leaf = Some(entry_id(model.next_entry));
                model.next_entry += 1;
                appended += 1;
            }
            2 => {
                let pair = assistant_pair(model.next_entry);
                journal.append(pair).await.expect("command appends");
                model.entries.push(entry_id(model.next_entry));
                model.entries.push(entry_id(model.next_entry + 1));
                model.leaf = Some(entry_id(model.next_entry + 1));
                model.next_entry += 2;
                model.turn_input += 1;
                model.turn_output += 1;
                appended += 2;
            }
            3 => {
                let pick = model.entries[rng.next(model.entries.len())];
                journal
                    .append(vec![Record::Leaf {
                        at: timestamp(),
                        to: Some(pick),
                    }])
                    .await
                    .expect("command appends");
                model.leaf = Some(pick);
                appended += 1;
            }
            4 => {
                let pick = model.entries[rng.next(model.entries.len())];
                let label = if rng.next(2) == 0 {
                    Some(format!("label-{}", rng.next(8)))
                } else {
                    None
                };
                journal
                    .append(vec![Record::Label {
                        at: timestamp(),
                        entry: pick,
                        label: label.clone().map(Into::into),
                    }])
                    .await
                    .expect("command appends");
                model.labels.insert(pick, label);
                appended += 1;
            }
            _ => {
                if model.open_turn.is_none() {
                    let turn = turn_id(model.next_turn);
                    journal
                        .append(vec![Record::TurnStart {
                            at: timestamp(),
                            turn,
                        }])
                        .await
                        .expect("command appends");
                    model.open_turn = Some(turn);
                    model.next_turn += 1;
                    model.turn_input = 0;
                    model.turn_output = 0;
                } else {
                    let turn = model.open_turn.expect("a turn is open");
                    let usage = (model.turn_input > 0 || model.turn_output > 0).then_some(Usage {
                        input_tokens: model.turn_input,
                        cached_input_tokens: 0,
                        output_tokens: model.turn_output,
                        reasoning_tokens: None,
                        cache_write_tokens: 0,
                        cost_usd: None,
                    });
                    journal
                        .append(vec![Record::TurnEnd {
                            at: timestamp(),
                            turn,
                            stop: TurnEndStop::Done,
                            usage,
                            changes: vec![],
                        }])
                        .await
                        .expect("command appends");
                    model.open_turn = None;
                }
                if rng.next(3) == 0 {
                    let archived = !model.archived;
                    journal
                        .append(vec![Record::Archive {
                            at: timestamp(),
                            archived,
                        }])
                        .await
                        .expect("command appends");
                    model.archived = archived;
                    appended += 2;
                } else {
                    appended += 1;
                }
            }
        }
        if step % 100 == 0 {
            assert_eq!(
                fold(journal.records()),
                model,
                "tree, leaf, labels, archive flag, and turn state agree at step {step}"
            );
        }
        if step % 2500 == 2499 {
            if let Some(turn) = model.open_turn {
                let usage = (model.turn_input > 0 || model.turn_output > 0).then_some(Usage {
                    input_tokens: model.turn_input,
                    cached_input_tokens: 0,
                    output_tokens: model.turn_output,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                });
                journal
                    .append(vec![Record::TurnEnd {
                        at: timestamp(),
                        turn,
                        stop: TurnEndStop::Done,
                        usage,
                        changes: vec![],
                    }])
                    .await
                    .expect("settle the open turn");
                model.open_turn = None;
                appended += 1;
            }
            journal.close().await.expect("session closes");
            let (mut reopened, _) = store.open_session(id).await.expect("session reopens");
            assert_eq!(
                fold(reopened.records()),
                model,
                "reopen preserves the model at step {step}"
            );
            reopens += 1;
            std::mem::swap(&mut journal, &mut reopened);
            reopened.close().await.expect("previous handle closes");
        }
    }
    assert_eq!(
        fold(journal.records()),
        model,
        "the final fold matches after 10000 commands"
    );
    if let Some(turn) = model.open_turn {
        let usage = (model.turn_input > 0 || model.turn_output > 0).then_some(Usage {
            input_tokens: model.turn_input,
            cached_input_tokens: 0,
            output_tokens: model.turn_output,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            cost_usd: None,
        });
        journal
            .append(vec![Record::TurnEnd {
                at: timestamp(),
                turn,
                stop: TurnEndStop::Done,
                usage,
                changes: vec![],
            }])
            .await
            .expect("settle the final turn");
        model.open_turn = None;
        appended += 1;
    }
    let durable = journal.records().len();
    journal.close().await.expect("session closes");
    let (reopened, _) = store.open_session(id).await.expect("session reopens");
    assert_eq!(
        fold(reopened.records()),
        model,
        "close and reopen preserve every command"
    );
    assert_eq!(
        reopened.records().len(),
        durable + 1,
        "only the recovery boot is added"
    );
    assert_eq!(
        appended + 2 + reopens,
        durable,
        "every command appended its records"
    );
}
