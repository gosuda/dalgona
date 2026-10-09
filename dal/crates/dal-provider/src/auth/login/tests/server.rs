use std::{
    future::pending,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinSet,
    time::timeout,
};

const ARRIVAL_GUARD: Duration = Duration::from_secs(20);
const ARRIVAL_CAPACITY: usize = 64;
const REQUEST_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub(super) struct Request {
    pub(super) method: String,
    pub(super) target: String,
    headers: Vec<(String, String)>,
    pub(super) body: String,
}

impl Request {
    pub(super) fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub(super) fn form(&self) -> Vec<(String, String)> {
        url::form_urlencoded::parse(self.body.as_bytes())
            .into_owned()
            .collect()
    }

    pub(super) fn field(&self, name: &str) -> Option<String> {
        self.form()
            .into_iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }
}

pub(super) enum Reply {
    Json(u16, String),
    Hold,
}

type Handler = Arc<dyn Fn(&Request) -> Reply + Send + Sync>;

pub(super) struct Server {
    base: String,
    log: Arc<Mutex<Vec<Request>>>,
    arrivals: mpsc::Receiver<Request>,
    _accept: JoinSet<()>,
}

impl Server {
    pub(super) async fn start(
        handler: impl Fn(&Request) -> Reply + Send + Sync + 'static,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let log = Arc::new(Mutex::new(Vec::new()));
        let (arrived, arrivals) = mpsc::channel(ARRIVAL_CAPACITY);
        let handler: Handler = Arc::new(handler);
        let mut accept = JoinSet::new();
        accept.spawn(accept_loop(listener, handler, Arc::clone(&log), arrived));
        Ok(Self {
            base,
            log,
            arrivals,
            _accept: accept,
        })
    }

    pub(super) fn base(&self) -> &str {
        &self.base
    }

    pub(super) fn requests(&self) -> Vec<Request> {
        self.log.lock().expect("request log").clone()
    }

    pub(super) fn requests_to(&self, path: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|request| request.path() == path)
            .collect()
    }

    pub(super) async fn next_request_to(&mut self, path: &str) -> Request {
        loop {
            let request = timeout(ARRIVAL_GUARD, self.arrivals.recv())
                .await
                .unwrap_or_else(|_| panic!("no request reached {path}"))
                .expect("test server stopped");
            if request.path() == path {
                return request;
            }
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    handler: Handler,
    log: Arc<Mutex<Vec<Request>>>,
    arrived: mpsc::Sender<Request>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { return };
                connections.spawn(serve(
                    stream,
                    Arc::clone(&handler),
                    Arc::clone(&log),
                    arrived.clone(),
                ));
            }
            Some(_finished) = connections.join_next() => {}
        }
    }
}

async fn serve(
    mut stream: TcpStream,
    handler: Handler,
    log: Arc<Mutex<Vec<Request>>>,
    arrived: mpsc::Sender<Request>,
) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let reply = handler(&request);
    log.lock().expect("request log").push(request.clone());
    let _sent = arrived.try_send(request);
    match reply {
        Reply::Json(status, body) => {
            let head = format!(
                "HTTP/1.1 {status} Test\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _written = stream.write_all(head.as_bytes()).await;
            let _written = stream.write_all(body.as_bytes()).await;
            let _closed = stream.shutdown().await;
        }
        Reply::Hold => pending::<()>().await,
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let count = stream.read(&mut chunk).await.ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
        let head_end = bytes.windows(4).position(|window| window == b"\r\n\r\n");
        let Some(head_end) = head_end else {
            if bytes.len() > REQUEST_LIMIT {
                return None;
            }
            continue;
        };
        let head = std::str::from_utf8(&bytes[..head_end]).ok()?;
        let mut lines = head.lines();
        let mut start = lines.next()?.split_whitespace();
        let method = start.next()?.to_owned();
        let target = start.next()?.to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
            .collect();
        let length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        if bytes.len() < head_end + 4 + length {
            if bytes.len() > REQUEST_LIMIT {
                return None;
            }
            continue;
        }
        let body = String::from_utf8(bytes[head_end + 4..head_end + 4 + length].to_vec()).ok()?;
        return Some(Request {
            method,
            target,
            headers,
            body,
        });
    }
}
