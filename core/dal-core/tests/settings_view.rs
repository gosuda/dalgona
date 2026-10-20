#![expect(clippy::expect_used, reason = "integration tests fail loudly")]

//! `Session::settings_view`, the one settings projection a live update, a
//! replayed journal, and a client view all share.
//!
//! A host seeds the replay with its configured default model. The seed is
//! what new rounds request, but only a journaled `Model` entry selects a
//! model, even one equal to the seed. Every other setting shows the value
//! the last record of its kind left.

use std::num::NonZeroU64;

use dal_core::{
    ApprovalMode, ClientId, Command, Effect, Entry, EntryId, EntryKind, Event, Family, JournalPart,
    Mode, ModelRoute, Record, Save, Session, Settings, SettingsView, ThinkingLevel, UpdateKind,
};

fn entry_id(value: u64) -> EntryId {
    EntryId::new(NonZeroU64::new(value).expect("nonzero entry id"))
}

fn entry(id: u64, kind: EntryKind) -> Entry {
    Entry {
        id: entry_id(id),
        parent: (id > 1).then(|| entry_id(id - 1)),
        at: jiff::Timestamp::UNIX_EPOCH,
        kind,
    }
}

fn user() -> Record {
    Record::User(entry(
        1,
        EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "hello".into(),
            }],
        },
    ))
}

fn route(model: &str, provider: Option<&str>) -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: model.into(),
        provider: provider.map(Into::into),
    }
}

fn default_route() -> ModelRoute {
    route("default-model", Some("acme"))
}

fn seed() -> Settings {
    Settings {
        model: Some(default_route()),
        thinking: ThinkingLevel::Medium,
        approval: ApprovalMode::Ask,
        name: None,
        mode: Mode::Normal,
    }
}

fn replay_with_effects(records: Vec<Record>) -> (Session, Vec<Effect>) {
    Session::replay_with(seed(), records, jiff::Timestamp::UNIX_EPOCH)
        .expect("fixture journal replays")
}

fn replay(records: Vec<Record>) -> Session {
    replay_with_effects(records).0
}

#[test]
fn a_seeded_default_is_requested_but_not_selected() {
    let session = replay(vec![
        user(),
        Record::Thinking(entry(
            2,
            EntryKind::Thinking {
                level: ThinkingLevel::Low,
            },
        )),
    ]);
    assert_eq!(
        session.requested_model(),
        Some(&default_route()),
        "new rounds still request the seeded default"
    );
    assert_eq!(
        session.settings_view(),
        SettingsView {
            model: None,
            thinking: ThinkingLevel::Low,
            approval: ApprovalMode::Ask,
            mode: Mode::Normal,
            name: None,
            scoped_models: None,
        }
    );
}

#[test]
fn a_model_record_selects_even_when_it_equals_the_seed() {
    let session = replay(vec![
        user(),
        Record::Model(entry(
            2,
            EntryKind::Model {
                route: default_route(),
            },
        )),
    ]);
    assert_eq!(session.settings_view().model, Some(default_route()));
}

#[test]
fn the_last_record_of_each_kind_wins_and_a_route_keeps_its_provider() {
    let last = route("gpt-4o", None);
    let session = replay(vec![
        user(),
        Record::Model(entry(
            2,
            EntryKind::Model {
                route: route("gpt-4o", Some("acme")),
            },
        )),
        Record::Model(entry(
            3,
            EntryKind::Model {
                route: last.clone(),
            },
        )),
        Record::Approval(entry(
            4,
            EntryKind::Approval {
                mode: ApprovalMode::All,
            },
        )),
        Record::Mode(entry(
            5,
            EntryKind::Mode {
                mode: Mode::EvalOnly,
            },
        )),
        Record::Name {
            at: jiff::Timestamp::UNIX_EPOCH,
            name: Some("first".into()),
        },
        Record::Name {
            at: jiff::Timestamp::UNIX_EPOCH,
            name: None,
        },
    ]);
    let view = session.settings_view();
    assert_eq!(view.model, Some(last), "a legacy route stays provider-less");
    assert_eq!(view.approval, ApprovalMode::All);
    assert_eq!(view.mode, Mode::EvalOnly);
    assert_eq!(view.name, None, "a cleared name stays cleared");
    assert_eq!(
        view.thinking,
        ThinkingLevel::Medium,
        "a setting no record touched keeps the seed"
    );
}

#[test]
fn a_journal_with_no_setting_records_shows_the_seed_without_a_model() {
    let view = replay(vec![user()]).settings_view();
    assert_eq!(view.model, None);
    assert_eq!(view.thinking, ThinkingLevel::Medium);
    assert_eq!(view.approval, ApprovalMode::Ask);
    assert_eq!(view.mode, Mode::Normal);
    assert_eq!(view.name, None);
}

fn commands(selecting: &[ModelRoute]) -> Vec<Command> {
    let mut commands = vec![Command::SetThinking {
        level: ThinkingLevel::Low,
        save: Save::SessionOnly,
    }];
    commands.extend(selecting.iter().map(|route| Command::SetModel {
        model: route.clone(),
        save: Save::SessionOnly,
    }));
    commands.extend([
        Command::SetApproval {
            mode: ApprovalMode::All,
            save: Save::SessionOnly,
        },
        Command::SetMode {
            mode: Mode::EvalFirst,
            save: Save::SessionOnly,
        },
        Command::Rename("renamed".into()),
    ]);
    commands
}

struct Live {
    session: Session,
    journal: Vec<Record>,
    published: Vec<SettingsView>,
}

impl Live {
    fn start() -> Self {
        let (session, effects) = replay_with_effects(Vec::new());
        let mut live = Self {
            session,
            journal: Vec::new(),
            published: Vec::new(),
        };
        live.collect(&effects);
        live
    }

    fn collect(&mut self, effects: &[Effect]) {
        for effect in effects {
            if let Effect::Emit(emit) = effect {
                self.journal.extend(emit.records.iter().cloned());
                self.published
                    .extend(emit.updates.iter().filter_map(|update| match update {
                        UpdateKind::Settings(view) => Some(view.clone()),
                        _ => None,
                    }));
            }
        }
    }

    fn submit(&mut self, cmd: Command) {
        let mut effects = Vec::new();
        self.session
            .step(
                Event::Command {
                    cmd,
                    by: ClientId::new("test"),
                },
                jiff::Timestamp::UNIX_EPOCH,
                &mut effects,
            )
            .expect("the command is accepted");
        self.collect(&effects);
    }
}

#[test]
fn a_replayed_journal_reports_the_view_its_live_session_published() {
    let cases = [
        (vec![], None),
        (vec![default_route()], Some(default_route())),
        (
            vec![route("first", None), route("second", Some("acme"))],
            Some(route("second", Some("acme"))),
        ),
    ];
    for (selecting, expected) in cases {
        let mut live = Live::start();
        for cmd in commands(&selecting) {
            live.submit(cmd);
        }

        let view = live.session.settings_view();
        assert_eq!(view.model, expected, "{selecting:?}");
        assert_eq!(live.published.last(), Some(&view), "{selecting:?}");

        let mut shown = vec![None];
        shown.extend(selecting.iter().cloned().map(Some));
        shown.extend(std::iter::repeat_n(expected, 3));
        assert_eq!(
            live.published
                .iter()
                .map(|update| update.model.clone())
                .collect::<Vec<_>>(),
            shown,
            "a model shows from its selection on, never before"
        );
        assert_eq!(replay(live.journal).settings_view(), view, "{selecting:?}");
    }
}
