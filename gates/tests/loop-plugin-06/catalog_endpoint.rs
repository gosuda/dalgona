//! Session-scoped loopback models-list endpoint for readiness gates.

use std::{io, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

pub(crate) struct CatalogEndpoint {
    base_url: String,
    server: JoinSet<io::Result<()>>,
}

impl CatalogEndpoint {
    pub(crate) async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let base_url = format!("http://{}", listener.local_addr()?);
        let mut server = JoinSet::new();
        server.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await?;
                tokio::time::timeout(Duration::from_secs(5), serve_one(stream))
                    .await
                    .map_err(io::Error::other)??;
            }
        });
        Ok(Self { base_url, server })
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) async fn close(mut self) -> io::Result<()> {
        self.server.abort_all();
        while let Some(result) = self.server.join_next().await {
            match result {
                Ok(result) => result?,
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        Ok(())
    }
}

async fn serve_one(stream: TcpStream) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    let body = r#"{"data":[{"id":"plain"}]}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len(),
    );
    reader.get_mut().write_all(response.as_bytes()).await
}
