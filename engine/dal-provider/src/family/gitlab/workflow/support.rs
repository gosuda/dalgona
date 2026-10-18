//! Loopback rig shared by the workflow surface tests and the provider-set
//! boundary test: a canned REST router, per-hop timeouts, and action frames.
//!
//! Test-only: the parent module declares this module under `#[cfg(test)]`.
//! Everything here speaks loopback TCP or builds in-memory fixtures; nothing
//! touches a live service or credential.

use std::sync::{Arc, Mutex};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use super::Timeouts;

/// Per-endpoint REST counts with the socket base one router serves.
#[derive(Debug, Default)]
pub(crate) struct RestCounts {
    pub(crate) creates: usize,
    pub(crate) stops: Vec<String>,
    pub(crate) direct_access: usize,
    pub(crate) ws_base: String,
}

async fn read_http(tcp: &mut tokio::net::TcpStream) -> (String, String, Vec<u8>) {
    let mut head = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        AsyncReadExt::read_exact(tcp, &mut byte).await.unwrap();
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head_text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head_text.lines();
    let start = lines.next().unwrap_or_default().to_owned();
    let len = head_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.trim().eq_ignore_ascii_case("content-length") {
                return None;
            }
            value.trim().parse::<usize>().ok()
        })
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    AsyncReadExt::read_exact(tcp, &mut body).await.unwrap();
    (start, head_text, body)
}

async fn reply(tcp: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    AsyncWriteExt::write_all(tcp, head.as_bytes())
        .await
        .unwrap();
    AsyncWriteExt::write_all(tcp, body.as_bytes())
        .await
        .unwrap();
}

/// Serves one canned REST scope per connection: settings no-ops, project
/// discovery, workflow `direct_access` pointing at the socket base, workflow
/// creates numbered `w-N`, stops recorded by id, and a GraphQL stub.
pub(crate) async fn rest_router(listener: TcpListener, counts: Arc<Mutex<RestCounts>>) {
    loop {
        let Ok((mut tcp, _)) = listener.accept().await else {
            return;
        };
        let counts = Arc::clone(&counts);
        let (start, _, _) = read_http(&mut tcp).await;
        let mut parts = start.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();
        let ws_base = lock(&counts).ws_base.clone();
        let body = if method == "PUT" {
            r"{}".to_owned()
        } else if path.starts_with("/api/v4/groups/") && path.contains("/projects") {
            r#"[{"id":11,"path_with_namespace":"g/p"}]"#.to_owned()
        } else if path.starts_with("/api/v4/ai/duo_workflows/direct_access") {
            lock(&counts).direct_access += 1;
            format!(r#"{{"duo_workflow_service":{{"token":"wtok","base_url":"{ws_base}"}}}}"#)
        } else if path.starts_with("/api/v4/ai/duo_workflows/workflows/") && method == "PATCH" {
            let id = path.rsplit('/').next().unwrap_or_default().to_owned();
            lock(&counts).stops.push(id);
            r"{}".to_owned()
        } else if path.starts_with("/api/v4/ai/duo_workflows/workflows") {
            let mut counts = lock(&counts);
            counts.creates += 1;
            format!(r#"{{"id":"w-{}"}}"#, counts.creates)
        } else if path.starts_with("/api/graphql") {
            r#"{"data":{"aiChatAvailableModels":{"pinnedModel":null}}}"#.to_owned()
        } else {
            r"{}".to_owned()
        };
        reply(&mut tcp, "200 OK", &body).await;
    }
}

/// Short per-hop deadlines so loopback driver tests restart fast.
pub(crate) fn test_timeouts() -> Timeouts {
    Timeouts {
        rest: std::time::Duration::from_secs(5),
        idle: std::time::Duration::from_millis(100),
    }
}

/// One `runMCPTool` server action frame with the given request id.
pub(crate) fn action_frame(id: &str) -> String {
    format!(
        r#"{{"action":{{"name":"runMCPTool","requestID":"{id}","args":{{"name":"read","args":{{"q":"x"}}}}}}}}"#
    )
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
