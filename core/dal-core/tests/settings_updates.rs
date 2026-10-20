#![expect(clippy::expect_used, reason = "integration tests fail loudly")]

//! The settings updates a live session publishes, through the public reducer.
//!
//! A host seeds the replay with its configured default model. That seed is
//! requested for new rounds but is not a selection, so no settings update
//! names it until a `SetModel` command selects a route.

use dal_core::{
    ApprovalMode, ClientId, Command, Effect, Event, Family, Mode, ModelRoute, Record, Save,
    Session, Settings, SettingsView, ThinkingLevel, UpdateKind,
};

fn default_route() -> ModelRoute {
    ModelRoute::Api {
        family: Family::Chat,
        model: "default-model".into(),
        provider: Some("acme".into()),
    }
}

fn inheriting_session() -> Session {
    let seed = Settings {
        model: Some(default_route()),
        thinking: ThinkingLevel::Medium,
        approval: ApprovalMode::Ask,
        name: None,
        mode: Mode::Normal,
    };
    Session::replay_with(seed, [], jiff::Timestamp::UNIX_EPOCH)
        .expect("an empty journal replays")
        .0
}

fn submit(session: &mut Session, cmd: Command) -> (SettingsView, Record) {
    let mut effects = Vec::new();
    session
        .step(
            Event::Command {
                cmd,
                by: ClientId::new("test"),
            },
            jiff::Timestamp::UNIX_EPOCH,
            &mut effects,
        )
        .expect("the command is accepted");
    let mut emitted = effects.into_iter().filter_map(|effect| match effect {
        Effect::Emit(emit) => Some(emit),
        _ => None,
    });
    let emit = emitted.next().expect("the command emits records");
    let view = emit
        .updates
        .into_iter()
        .find_map(|update| match update {
            UpdateKind::Settings(view) => Some(view),
            _ => None,
        })
        .expect("the command publishes settings");
    let record = emit.records.into_iter().next().expect("a record");
    (view, record)
}

#[test]
fn changing_another_setting_does_not_select_the_inherited_default() {
    let mut session = inheriting_session();
    let commands = [
        Command::SetThinking {
            level: ThinkingLevel::Low,
            save: Save::SessionOnly,
        },
        Command::SetApproval {
            mode: ApprovalMode::All,
            save: Save::SessionOnly,
        },
        Command::SetMode {
            mode: Mode::EvalOnly,
            save: Save::SessionOnly,
        },
        Command::Rename("renamed".into()),
    ];
    for cmd in commands {
        let (view, _) = submit(&mut session, cmd.clone());
        assert_eq!(view.model, None, "{cmd:?} selected the inherited default");
    }
    assert_eq!(
        session.requested_model(),
        Some(&default_route()),
        "new rounds still request the inherited default"
    );
}

#[test]
fn a_selected_route_publishes_and_keeps_publishing_even_when_it_equals_the_default() {
    let mut session = inheriting_session();
    let (view, record) = submit(
        &mut session,
        Command::SetModel {
            model: default_route(),
            save: Save::SessionOnly,
        },
    );
    assert!(matches!(record, Record::Model(_)));
    assert_eq!(view.model, Some(default_route()));

    let (view, _) = submit(
        &mut session,
        Command::SetThinking {
            level: ThinkingLevel::Low,
            save: Save::SessionOnly,
        },
    );
    assert_eq!(
        view.model,
        Some(default_route()),
        "a later settings update keeps the selection"
    );

    let other = ModelRoute::Api {
        family: Family::Chat,
        model: "other-model".into(),
        provider: None,
    };
    let (view, _) = submit(
        &mut session,
        Command::SetModel {
            model: other.clone(),
            save: Save::SessionOnly,
        },
    );
    assert_eq!(view.model, Some(other), "a provider-less route stays so");
}
