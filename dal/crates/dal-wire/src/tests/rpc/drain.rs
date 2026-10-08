//! A draining connection answers new requests with `-32009` instead of running
//! them, and a host that is shut down maps to the same code.

use std::time::Duration;

use dal_agent::HostError;
use sonic_rs::JsonValueTrait;
use tokio_util::sync::CancellationToken;

use super::{Rpc, assert_error, initialize, rig};
use crate::rpc::host_error;
use crate::serve_rpc_draining;
use crate::transport::MemoryTransport;

const DRAINING_TEXT: &str = "the server is shutting down and accepts no new requests";
const DRAINING_HINT: &str = "Wait for the server to start again, then reconnect.";

#[tokio::test]
async fn a_draining_connection_rejects_new_requests_with_server_draining() {
    let rig = rig(&[]).await;
    let drain = CancellationToken::new();
    let (transport, peer) = MemoryTransport::pair(64);
    let server = serve_rpc_draining(rig.host.clone(), transport, drain.clone());
    let client = async {
        let mut rpc = Rpc::new(peer);
        initialize(&mut rpc).await;
        let before = rpc.call(1, "session/list", sonic_rs::json!({})).await;
        assert!(
            before.get("error").is_none(),
            "served before drain: {before}"
        );

        drain.cancel();
        let reply = rpc.call(2, "session/list", sonic_rs::json!({})).await;
        assert_error(&reply, -32009, DRAINING_TEXT);
        assert_eq!(reply["error"]["data"]["hint"].as_str(), Some(DRAINING_HINT));
    };
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(server, client)
    })
    .await
    .expect("draining connection ends");
    outcome.expect("rpc serve ends cleanly");
}

#[tokio::test]
async fn a_draining_connection_closes_after_the_grace_period_without_a_client_hangup() {
    let rig = rig(&[]).await;
    let drain = CancellationToken::new();
    let (transport, peer) = MemoryTransport::pair(64);
    let (ended_tx, ended_rx) = tokio::sync::oneshot::channel();
    let server = async {
        let outcome = serve_rpc_draining(rig.host.clone(), transport, drain.clone()).await;
        let _ = ended_tx.send(());
        outcome
    };
    let client = async {
        let mut rpc = Rpc::new(peer);
        initialize(&mut rpc).await;
        drain.cancel();
        // The client stays connected; the server must still end on its own.
        tokio::time::timeout(Duration::from_secs(5), ended_rx)
            .await
            .expect("server closes within the grace period")
            .expect("server reports its end");
        drop(rpc);
    };
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(server, client)
    })
    .await
    .expect("draining connection ends");
    outcome.expect("rpc serve ends cleanly");
}

#[test]
fn a_host_that_is_shut_down_maps_to_server_draining() {
    let error = host_error(HostError::Closed);
    assert_eq!(error.code, -32009);
    assert_eq!(error.message, DRAINING_TEXT);
}
