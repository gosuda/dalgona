use dal_agent::SessionRef;
use dal_core::{BlobId, ClientId, Command, Expect, Part, Stop, TurnId, UpdateKind};
use sonic_rs::{JsonContainerTrait, JsonValueTrait};

use super::super::support::{
    Rig, WAIT, assert_error, find_dirs, gate_step, initialize, result, rig, text_step,
};
use super::{open, prompt, until_update, with_rpc};

#[tokio::test]
async fn turn_mismatch_errors() {
    let rig = rig(&[gate_step("c1"), text_step(&["done"], 1, 1)]).await;
    let ws = rig.ws();
    let gate = rig.gate.clone();
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let session = open(&mut rpc, 1, sonic_rs::json!({"type": "new", "workspace": ws})).await;
        let first = rpc
            .call(
                2,
                "session/submit",
                sonic_rs::json!({"sessionId": session, "command": prompt("wait", Expect::Idle)}),
            )
            .await;
        let turn: TurnId =
            sonic_rs::from_value(&result(&first)["turn"]).expect("accepted turn id");
        let busy = rpc
            .call(
                3,
                "session/submit",
                sonic_rs::json!({"sessionId": session, "command": prompt("again", Expect::Idle)}),
            )
            .await;
        assert_error(
            &busy,
            -32004,
            &format!("expected an idle session, actual turn {turn} running"),
        );
        let wrong = TurnId::new(std::num::NonZeroU64::new(turn.get() + 7).expect("nonzero"));
        let mismatch = rpc
            .call(
                4,
                "session/submit",
                sonic_rs::json!({"sessionId": session, "command": prompt("after", Expect::After(wrong))}),
            )
            .await;
        assert_error(
            &mismatch,
            -32004,
            &format!("expected turn {wrong}, actual turn {turn}"),
        );
        gate.add_permits(1);
    })
    .await;
}

/// Runs one prompt through a direct agent and waits for its end.
async fn run_turn(agent: &dal_agent::Agent, text: &str) {
    let mut subscription = agent.subscribe(None).expect("subscribe");
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await
        .expect("prompt accepted");
    loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .expect("update in time")
            .expect("subscription open");
        if let dal_agent::Delivery::Update(update) = delivery
            && matches!(update.kind, UpdateKind::TurnEnded { .. })
        {
            return;
        }
    }
}

/// Opens a durable session, runs two turns, and returns its hold, id, and head.
async fn seed_two_turns(rig: &Rig) -> (dal_agent::Agent, String, u64, u64) {
    let agent = rig
        .host
        .open(
            SessionRef::New {
                workspace: rig.core_workspace(),
                name: None,
            },
            ClientId::new("seed"),
        )
        .await
        .expect("session opens");
    run_turn(&agent, "one").await;
    run_turn(&agent, "two").await;
    let head = agent.view(dal_core::PageReq::default()).expect("view");
    let seq = head.seq.get();
    assert!(
        seq > 10,
        "two turns produce more than ten updates, got {seq}"
    );
    (agent, head.session.id.to_string(), head.r#gen.get(), seq)
}

#[tokio::test]
async fn replay_window_edges() {
    let steps = [
        text_step(&["a", "b", "c", "d", "e"], 1, 1),
        text_step(&["f", "g", "h", "i", "j"], 1, 1),
        text_step(&["live"], 1, 1),
    ];
    let rig = rig(&steps).await;
    let (seed, session, r#gen, seq) = seed_two_turns(&rig).await;
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let beyond = rpc
            .call(
                1,
                "session/subscribe",
                sonic_rs::json!({"sessionId": session, "gen": r#gen, "after": seq + 1}),
            )
            .await;
        assert_error(
            &beyond,
            -32602,
            &format!(
                "invalid params for session/subscribe: after {} is beyond seq {seq}",
                seq + 1
            ),
        );
        rpc.send(
            2,
            "session/subscribe",
            sonic_rs::json!({"sessionId": session, "gen": r#gen, "after": seq - 10}),
        )
        .await;
        let reply = rpc.next().await;
        assert_eq!(
            reply["id"].as_i64(),
            Some(2),
            "reply precedes replay: {reply}"
        );
        assert_eq!(result(&reply)["seq"].as_u64(), Some(seq));
        let mut replayed = Vec::new();
        for _ in 0..10 {
            let frame = rpc.next().await;
            assert_eq!(frame["method"].as_str(), Some("session/update"), "{frame}");
            replayed.push(frame["params"]["seq"].as_u64().expect("seq"));
        }
        assert_eq!(replayed, ((seq - 9)..=seq).collect::<Vec<_>>());
        rpc.send(
            3,
            "session/submit",
            sonic_rs::json!({"sessionId": session, "command": prompt("three", Expect::Idle)}),
        )
        .await;
        let live = until_update(&mut rpc, "turn_ended").await;
        let seqs: Vec<u64> = live
            .iter()
            .map(|frame| frame["params"]["seq"].as_u64().expect("seq"))
            .collect();
        assert_eq!(
            seqs.first().copied(),
            Some(seq + 1),
            "live resumes after head: {seqs:?}"
        );
        assert!(
            seqs.windows(2).all(|pair| pair[0] < pair[1]),
            "duplicates: {seqs:?}"
        );
        rpc.send(
            4,
            "session/subscribe",
            sonic_rs::json!({"sessionId": session, "gen": r#gen + 1, "after": 1}),
        )
        .await;
        let stale = loop {
            let frame = rpc.next().await;
            if frame["params"]["update"]["type"].as_str() == Some("resync") {
                break frame;
            }
        };
        assert_eq!(
            stale["params"]["sessionId"].as_str(),
            Some(session.as_str())
        );
    })
    .await;
    drop(seed);
}

#[tokio::test]
async fn blob_read_errors() {
    let rig = rig(&[text_step(&["stored"], 1, 1)]).await;
    let data = rig.data();
    let seed = rig
        .host
        .open(
            SessionRef::New {
                workspace: rig.core_workspace(),
                name: None,
            },
            ClientId::new("seed"),
        )
        .await
        .expect("session opens");
    run_turn(&seed, "persist").await;
    let session = seed
        .view(dal_core::PageReq::default())
        .expect("view")
        .session
        .id
        .to_string();
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let blob = BlobId::from_bytes(b"missing").to_string();
        let unknown = rpc
            .call(
                2,
                "blob/read",
                sonic_rs::json!({"sessionId": session, "blobId": blob}),
            )
            .await;
        assert_error(
            &unknown,
            -32002,
            &format!("blob {blob} was not found in session {session}"),
        );
        let dirs = find_dirs(&data, &session);
        assert_eq!(dirs.len(), 1, "one store directory per session: {dirs:?}");
        std::fs::remove_dir_all(&dirs[0]).expect("delete session directory");
        let deleted = rpc
            .call(
                3,
                "blob/read",
                sonic_rs::json!({"sessionId": session, "blobId": blob}),
            )
            .await;
        assert_error(&deleted, -32003, &format!("session {session} was deleted"));
    })
    .await;
    drop(seed);
}

#[tokio::test]
async fn docs_read_errors() {
    let rig = rig(&[]).await;
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        let list = rpc.call(1, "docs/read", sonic_rs::json!({})).await;
        let uris: Vec<&str> = result(&list)["documents"]
            .as_array()
            .expect("document list")
            .iter()
            .filter_map(|row| row["uri"].as_str())
            .collect();
        let protocol = format!("{}://protocol", "dal");
        assert!(uris.contains(&protocol.as_str()), "{uris:?}");
        let nope = format!("{}://nope", "dal");
        let missing = rpc
            .call(2, "docs/read", sonic_rs::json!({"uri": nope.as_str()}))
            .await;
        assert_error(&missing, -32002, &format!("no document at {nope}"));
        let bare = rpc
            .call(3, "docs/read", sonic_rs::json!({"uri": "config"}))
            .await;
        assert_error(
            &bare,
            -32602,
            "invalid params for docs/read: \"config\" is not a document URI",
        );
    })
    .await;
}

#[tokio::test]
async fn disconnect_keeps_turn() {
    let rig = rig(&[gate_step("c1"), text_step(&["after"], 1, 1)]).await;
    let ws = rig.ws();
    let mut ids = Vec::new();
    with_rpc(&rig, async |mut rpc| {
        initialize(&mut rpc).await;
        for id in 1..=3 {
            ids.push(
                open(
                    &mut rpc,
                    id,
                    sonic_rs::json!({"type": "new", "workspace": ws.as_str()}),
                )
                .await,
            );
        }
        let reply = rpc
            .call(
                4,
                "session/submit",
                sonic_rs::json!({"sessionId": ids[0], "command": prompt("wait", Expect::Idle)}),
            )
            .await;
        assert_eq!(result(&reply)["type"].as_str(), Some("accepted"));
    })
    .await;
    let agent = rig
        .host
        .open(
            SessionRef::Resume {
                key: ids[0].as_str().into(),
                workspace: rig.core_workspace(),
            },
            ClientId::new("observer"),
        )
        .await
        .expect("session stays open after disconnect");
    let head = agent.view(dal_core::PageReq::default()).expect("view");
    assert!(
        matches!(head.turn, dal_core::TurnState::Running { .. }),
        "disconnect cancelled the turn: {:?}",
        head.turn
    );
    let mut subscription = agent
        .subscribe(Some((head.r#gen, head.seq)))
        .expect("subscribe");
    rig.gate.add_permits(1);
    let stop = loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .expect("update in time")
            .expect("subscription open");
        if let dal_agent::Delivery::Update(update) = delivery
            && let UpdateKind::TurnEnded { stop, .. } = update.kind
        {
            break stop;
        }
    };
    assert!(matches!(stop, Stop::EndTurn), "{stop:?}");
    for id in &ids[1..] {
        let key: Box<str> = id.as_str().into();
        rig.host
            .open(
                SessionRef::Resume {
                    key,
                    workspace: rig.core_workspace(),
                },
                ClientId::new("observer"),
            )
            .await
            .expect("released session reopens");
    }
}
