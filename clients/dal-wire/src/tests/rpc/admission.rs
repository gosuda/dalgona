//! In-flight RPC admission limits apply to handler lifetimes across transports.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use super::super::support::{Rpc, initialize, result, rig};
use crate::rpc::{MAX_IN_FLIGHT, frame_charge, reserve_frame, serve_rpc_with_budget};
use crate::transport::MemoryTransport;

#[test]
fn structural_json_charge_ignores_punctuation_inside_strings() {
    let plain = r#"{"params":"abc"}"#;
    let punctuation = r#"{"params":"[,]"}"#;
    assert_eq!(frame_charge(plain), frame_charge(punctuation));

    let escaped_plain = r#"{"params":"\"abc"}"#;
    let escaped_punctuation = r#"{"params":"\"[,]"}"#;
    assert_eq!(
        frame_charge(escaped_plain),
        frame_charge(escaped_punctuation)
    );

    let dense = format!("[{}]", "0,".repeat(128).trim_end_matches(','));
    let text = format!(r#""{}""#, "x".repeat(dense.len() - 2));
    assert!(
        frame_charge(&dense).expect("dense cost") > frame_charge(&text).expect("text cost"),
        "short numeric arrays allocate more decoded nodes than a string"
    );
}

#[test]
fn frame_permits_enforce_connection_and_process_limits_and_release_on_drop() {
    let frame = r#"{"jsonrpc":"2.0","id":1,"method":"host/subscribe","params":{}}"#;
    let charge = usize::try_from(frame_charge(frame).expect("frame charge")).expect("usize");
    let global = Arc::new(Semaphore::new(charge * 2));
    let first_conn = Arc::new(Semaphore::new(charge));
    let second_conn = Arc::new(Semaphore::new(charge));
    let third_conn = Arc::new(Semaphore::new(charge));

    let first = reserve_frame(frame, &first_conn, &global).expect("first admitted");
    assert!(
        reserve_frame(frame, &first_conn, &global).is_none(),
        "connection admission must not exceed its budget"
    );
    let second = reserve_frame(frame, &second_conn, &global).expect("second admitted");
    assert_eq!(global.available_permits(), 0);
    assert!(
        reserve_frame(frame, &third_conn, &global).is_none(),
        "connections must share the same process budget"
    );

    drop(first);
    assert_eq!(first_conn.available_permits(), charge);
    assert_eq!(global.available_permits(), charge);
    let third = reserve_frame(frame, &third_conn, &global).expect("freed budget is reusable");
    assert_eq!(global.available_permits(), 0);
    drop(second);
    drop(third);
    assert_eq!(global.available_permits(), charge * 2);
}

#[tokio::test]
async fn a_connection_rejects_a_second_unaffordable_pending_subscription() {
    const GLOBAL: usize = 1_000_000;
    const CONNECTION: usize = 400_000;

    let rig = rig(&[]).await;
    let global = Arc::new(Semaphore::new(GLOBAL));
    let (transport, peer) = MemoryTransport::pair(8);
    let server = serve_rpc_with_budget(
        rig.host.clone(),
        transport,
        CancellationToken::new(),
        Arc::clone(&global),
        CONNECTION,
    );
    let client = async {
        let mut rpc = Rpc::new(peer);
        initialize(&mut rpc).await;
        let params = sonic_rs::json!({"padding": "x".repeat(140_000)});
        result(&rpc.call(1, "host/subscribe", params.clone()).await);
        assert!(
            global.available_permits() < GLOBAL,
            "the subscribed handler must retain its memory charge"
        );

        rpc.send(2, "host/subscribe", params).await;
        let (_incoming, mut outgoing) = rpc.into_parts();
        assert!(
            tokio::time::timeout(Duration::from_secs(4), outgoing.recv())
                .await
                .expect("over-budget connection closes")
                .is_none(),
            "the server must not dispatch a second unaffordable request"
        );
    };

    let (outcome, ()) = tokio::join!(server, client);
    outcome.expect("connection ends cleanly on overload");
    assert_eq!(global.available_permits(), GLOBAL);
}

#[tokio::test]
async fn distinct_connections_share_budget_until_the_handler_completes() {
    const GLOBAL: usize = 400_000;
    let rig = rig(&[]).await;
    let global = Arc::new(Semaphore::new(GLOBAL));
    let (left_transport, left_peer) = MemoryTransport::pair(8);
    let (right_transport, right_peer) = MemoryTransport::pair(8);
    let left_server = serve_rpc_with_budget(
        rig.host.clone(),
        left_transport,
        CancellationToken::new(),
        Arc::clone(&global),
        GLOBAL,
    );
    let right_server = serve_rpc_with_budget(
        rig.host.clone(),
        right_transport,
        CancellationToken::new(),
        Arc::clone(&global),
        GLOBAL,
    );
    let client = async {
        let mut left = Rpc::new(left_peer);
        let mut right = Rpc::new(right_peer);
        initialize(&mut left).await;
        initialize(&mut right).await;
        assert_eq!(global.available_permits(), GLOBAL);

        let params = sonic_rs::json!({"padding": "x".repeat(140_000)});
        result(&left.call(1, "host/subscribe", params.clone()).await);
        let remaining = global.available_permits();
        assert!(remaining > 0 && remaining < GLOBAL / 2);

        right.send(2, "host/subscribe", params).await;
        let (_incoming, mut outgoing) = right.into_parts();
        assert!(
            tokio::time::timeout(Duration::from_secs(4), outgoing.recv())
                .await
                .expect("second connection reaches budget rejection")
                .is_none(),
            "the shared budget must prevent a second large handler"
        );

        result(&left.call(3, "commands/list", sonic_rs::json!({})).await);
        assert_eq!(
            global.available_permits(),
            remaining,
            "an unrelated request must not release the subscription charge"
        );
        result(&left.call(4, "host/unsubscribe", sonic_rs::json!({})).await);
        tokio::time::timeout(Duration::from_secs(2), async {
            while global.available_permits() != GLOBAL {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the handler releases its permits after unsubscribe");
    };

    let (left_outcome, right_outcome, ()) = tokio::join!(left_server, right_server, client);
    left_outcome.expect("first connection ends cleanly");
    right_outcome.expect("over-budget connection ends cleanly");
}

#[tokio::test]
async fn a_full_request_count_still_accepts_cancellation() {
    let rig = rig(&[]).await;
    let global = Arc::new(Semaphore::new(8 * 1024 * 1024));
    let (transport, peer) = MemoryTransport::pair(MAX_IN_FLIGHT + 4);
    let server = serve_rpc_with_budget(
        rig.host.clone(),
        transport,
        CancellationToken::new(),
        Arc::clone(&global),
        8 * 1024 * 1024,
    );
    let client = async {
        let mut rpc = Rpc::new(peer);
        initialize(&mut rpc).await;
        for id in 1..=MAX_IN_FLIGHT {
            result(
                &rpc.call(
                    i64::try_from(id).expect("request id fits"),
                    "host/subscribe",
                    sonic_rs::json!({}),
                )
                .await,
            );
        }

        rpc.notify("$/cancel_request", sonic_rs::json!({"requestId": 1}))
            .await;
        result(&rpc.call(1000, "commands/list", sonic_rs::json!({})).await);
    };
    let (outcome, ()) = tokio::join!(server, client);
    outcome.expect("saturated connection ends after handling cancellation");
}
