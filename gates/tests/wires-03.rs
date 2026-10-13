//! Unknown update variants map to fallbacks while the turn continues.
#[expect(
    dead_code,
    reason = "gate support helpers are shared across independent test targets"
)]
mod support;

use std::{error::Error, time::Duration};

use dal_agent::SessionRef;
use dal_core::{Stop, UpdateKind, Workspace};
use dal_wire::{RemoteDelivery, RemoteEndpoint, RemoteHost};
use futures::{SinkExt, StreamExt};
use sonic_rs::{JsonValueTrait, Value};
use tokio::net::TcpListener;

const SESSION: &str = "018f0f62-3b00-7000-8000-000000000001";
const WAIT: Duration = Duration::from_secs(10);

async fn serve_unknown_then_ended(
    listener: &TcpListener,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (stream, _) = listener.accept().await?;
    let mut ws = tokio_tungstenite::accept_async(stream).await?;
    for _ in 0..3 {
        let msg = tokio::time::timeout(WAIT, ws.next())
            .await?
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "client closed handshake")
            })??;
        let text = match msg {
            tokio_tungstenite::tungstenite::Message::Text(text) => text.to_string(),
            other => {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("expected text request, got {other:?}"),
                )));
            }
        };
        let frame: Value = sonic_rs::from_str(&text)?;
        let method = frame
            .get("method")
            .and_then(|m| m.as_str())
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "request has a method")
            })?;
        let id = frame.get("id").cloned().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "request has an id")
        })?;
        let result = match method {
            "initialize" => sonic_rs::json!({"protocolVersion": 1}),
            "session/open" => sonic_rs::json!({"sessionId": SESSION, "gen": 1, "view": {}}),
            "session/subscribe" => sonic_rs::json!({"gen": 1, "seq": 5}),
            _ => {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unexpected method {method}"),
                )));
            }
        };
        let reply = sonic_rs::to_string(&sonic_rs::json!({
            "jsonrpc": "2.0", "id": id, "result": result,
        }))?;
        ws.send(tokio_tungstenite::tungstenite::Message::Text(reply.into()))
            .await?;
    }
    for (seq, update) in [
        (
            6,
            sonic_rs::json!({"type": "future_update", "payload": {"value": 7}}),
        ),
        (
            7,
            sonic_rs::json!({"type": "turn_ended", "turn": 1, "stop": "end_turn"}),
        ),
    ] {
        let note = sonic_rs::to_string(&sonic_rs::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": SESSION, "gen": 1, "seq": seq, "update": update},
        }))?;
        ws.send(tokio_tungstenite::tungstenite::Message::Text(note.into()))
            .await?;
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(msg) = ws.next().await {
            if msg.is_err() {
                break;
            }
        }
    })
    .await;
    Ok(())
}

async fn drive_unknown_then_ended(
    endpoint: RemoteEndpoint,
    workspace: &std::path::Path,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let host = RemoteHost::connect(endpoint).await?;
    let agent = host
        .open(SessionRef::Continue {
            workspace: Workspace::new(workspace.to_path_buf())?,
        })
        .await?;
    let mut subscription = agent.subscribe(None).await?;
    let first: RemoteDelivery = tokio::time::timeout(WAIT, subscription.next()).await??;
    let second: RemoteDelivery = tokio::time::timeout(WAIT, subscription.next()).await??;
    let RemoteDelivery::Update(first_update) = &first else {
        return Err(
            format!("expected an update delivery for the unknown kind, got {first:?}").into(),
        );
    };
    assert!(
        matches!(first_update.kind, UpdateKind::Unknown),
        "unknown wire type must arrive as a fallback instead of stalling: {first_update:?}"
    );
    assert_eq!(
        (first_update.r#gen.get(), first_update.seq.get()),
        (1, 6),
        "fallback keeps its wire cursor: {first_update:?}"
    );
    let RemoteDelivery::Update(second_update) = &second else {
        return Err(
            format!("expected an update delivery after the fallback, got {second:?}").into(),
        );
    };
    assert!(
        matches!(
            second_update.kind,
            UpdateKind::TurnEnded {
                stop: Stop::EndTurn,
                ..
            }
        ),
        "valid terminal update still completes the turn: {second_update:?}"
    );
    assert_eq!(
        (second_update.r#gen.get(), second_update.seq.get()),
        (1, 7),
        "terminal update keeps its wire cursor: {second_update:?}"
    );
    assert!(
        (second_update.r#gen.get(), second_update.seq.get())
            > (first_update.r#gen.get(), first_update.seq.get()),
        "turn completes in order after the fallback: {first_update:?} then {second_update:?}"
    );
    drop(subscription);
    drop(agent);
    drop(host);
    Ok(())
}

#[tokio::test]
async fn unknown_update_variant_maps_to_fallback_and_turn_continues()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let workspace_dir = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let endpoint =
        RemoteEndpoint::WebSocket(format!("ws://127.0.0.1:{}/", addr.port()).parse().map_err(
            |e| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("ws url: {e:?}")),
        )?);
    let (server_outcome, client_outcome) = tokio::time::timeout(WAIT, async {
        tokio::join!(
            serve_unknown_then_ended(&listener),
            drive_unknown_then_ended(endpoint, workspace_dir.path())
        )
    })
    .await?;
    server_outcome?;
    client_outcome?;
    Ok(())
}
