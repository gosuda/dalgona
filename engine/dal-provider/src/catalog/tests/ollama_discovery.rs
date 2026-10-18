//! Dynamic discovery of the native Ollama wire: `GET api/tags` joined to the
//! `POST api/show` capability probe, against a scripted loopback server that
//! records every request it answers.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use dal_core::ThinkingLevel::{High, Low, Max, Medium};

use super::*;

/// One request the scripted server answered.
struct Captured {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    /// The `model` member of a JSON request body.
    fn model(&self) -> Option<String> {
        let body: sonic_rs::Value = sonic_rs::from_str(&self.body).ok()?;
        sonic_rs::JsonValueTrait::as_str(&body["model"]).map(str::to_owned)
    }
}

/// What the script saw: every request, and the most requests answered at once.
struct Seen {
    requests: Vec<Captured>,
    max_in_flight: usize,
}

struct Script {
    port: u16,
    handle: thread::JoinHandle<Seen>,
}

impl Script {
    fn finish(self) -> Seen {
        self.handle.join().expect("the scripted server completes")
    }
}

fn read_request(stream: &mut TcpStream) -> Captured {
    stream
        .set_nonblocking(false)
        .expect("make the replay socket blocking");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set the replay socket read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("clone the replay socket"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("read the request line");
    let mut parts = line.split_whitespace();
    let method = parts.next().expect("a method").to_owned();
    let path = parts.next().expect("a path").to_owned();
    let mut headers = Vec::new();
    let mut length = 0_usize;
    loop {
        let mut header_line = String::new();
        let bytes = reader
            .read_line(&mut header_line)
            .expect("read a request header");
        if bytes == 0 || header_line == "\r\n" {
            break;
        }
        let (name, value) = header_line.split_once(':').expect("a header has a colon");
        let name = name.trim().to_ascii_lowercase();
        if name == "content-length" {
            length = value.trim().parse().expect("a content length");
        }
        headers.push((name, value.trim().to_owned()));
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body).expect("read the request body");
    Captured {
        method,
        path,
        headers,
        body: String::from_utf8(body).expect("a UTF-8 request body"),
    }
}

/// Answers exactly `expected` requests on one loopback port, each on its own
/// thread, holding every reply for `hold` so overlapping probes overlap.
fn serve<F>(expected: usize, hold: Duration, route: F) -> Script
where
    F: Fn(&Captured) -> (u16, String) + Send + Sync + 'static,
{
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the scripted listener");
    listener
        .set_nonblocking(true)
        .expect("make the scripted listener nonblocking");
    let port = listener
        .local_addr()
        .expect("read the listener port")
        .port();
    let route = Arc::new(route);
    let log = Arc::new(Mutex::new(Vec::new()));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut workers = Vec::new();
        while workers.len() < expected {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let route = Arc::clone(&route);
                    let log = Arc::clone(&log);
                    let in_flight = Arc::clone(&in_flight);
                    let max_in_flight = Arc::clone(&max_in_flight);
                    workers.push(thread::spawn(move || {
                        let request = read_request(&mut stream);
                        let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        max_in_flight.fetch_max(now, Ordering::SeqCst);
                        thread::sleep(hold);
                        let (status, body) = route(&request);
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                        let reply = format!(
                            "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        stream
                            .write_all(reply.as_bytes())
                            .expect("write the scripted reply");
                        log.lock().expect("lock the request log").push(request);
                    }));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "the scripted server saw {} of {expected} requests",
                        workers.len()
                    );
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
        for worker in workers {
            worker.join().expect("answer one scripted request");
        }
        let requests = std::mem::take(&mut *log.lock().expect("lock the request log"));
        Seen {
            requests,
            max_in_flight: max_in_flight.load(Ordering::SeqCst),
        }
    });
    Script { port, handle }
}

/// The standard Ollama script: `tags` for `GET /api/tags`, then one scripted
/// `(status, body)` per model for `POST /api/show`; an unscripted model is a
/// 404.
fn ollama_route(
    tags: &'static str,
    shows: &'static [(&'static str, u16, &'static str)],
) -> impl Fn(&Captured) -> (u16, String) + Send + Sync + 'static {
    move |request| {
        if request.method == "GET" && request.path == "/api/tags" {
            return (200, tags.to_owned());
        }
        if request.method == "POST" && request.path == "/api/show" {
            let model = request.model();
            return shows
                .iter()
                .find(|(id, ..)| Some(*id) == model.as_deref())
                .map_or(
                    (404, String::from(r#"{"error":"model not found"}"#)),
                    |(_, status, body)| (*status, (*body).to_owned()),
                );
        }
        (404, String::from("{}"))
    }
}

fn ollama_provider(id: &str, port: u16, path: &str, auth: AuthStyle) -> ProviderEntry {
    let mut entry = provider(
        id,
        Family::Ollama,
        &format!("http://127.0.0.1:{port}{path}"),
    );
    entry.auth = auth;
    entry
}

async fn refresh(provider: &ProviderEntry, credential: &Credential, cache: &Path) -> CatalogFetch {
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider,
        credential,
        cache_dir: cache,
        user_agent: "dalgon/test",
        version: "test",
    };
    load_models(&fetch, |_| std::future::pending::<()>()).await
}

fn openai_ladder(entry: &CatalogEntry) -> (Vec<dal_core::ThinkingLevel>, bool) {
    match &entry.thinking {
        ThinkingSupport::OpenAi {
            accepted,
            none_supported,
        } => (accepted.clone(), *none_supported),
        other => panic!("{} carries {other:?}", entry.id),
    }
}

fn row_of<'a>(fetched: &'a CatalogFetch, id: &str) -> &'a CatalogEntry {
    fetched
        .entries
        .iter()
        .find(|entry| entry.id.as_ref() == id)
        .unwrap_or_else(|| panic!("no row for {id}"))
}

const LOCAL_TAGS: &str = r#"{"models":[{"name":"gpt-oss:20b","model":"gpt-oss:20b"},{"name":"llava:7b"},{"name":"nomic-embed:1"}]}"#;

const LOCAL_SHOWS: &[(&str, u16, &str)] = &[
    (
        "gpt-oss:20b",
        200,
        r#"{"license":"x","capabilities":["completion","tools","thinking"],"model_info":{"general.architecture":"gptoss","gptoss.context_length":131072,"gptoss.block_count":24},"details":{"family":"gptoss"}}"#,
    ),
    (
        "llava:7b",
        200,
        r#"{"capabilities":["completion","vision","tools"],"model_info":{"general.architecture":"llama","llama.context_length":4096}}"#,
    ),
    (
        "nomic-embed:1",
        200,
        r#"{"capabilities":["embedding"],"model_info":{"nomic-bert.context_length":2048}}"#,
    ),
];

#[tokio::test]
async fn local_discovery_joins_tags_to_the_show_probe_without_credentials() {
    let directory = TestDir::new();
    let script = serve(
        1 + LOCAL_SHOWS.len(),
        Duration::ZERO,
        ollama_route(LOCAL_TAGS, LOCAL_SHOWS),
    );
    let provider = ollama_provider("ollama", script.port, "/api/", AuthStyle::None);
    let fetched = refresh(&provider, &Credential::None, directory.path()).await;
    let seen = script.finish();
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );

    let thinker = row_of(&fetched, "gpt-oss:20b");
    assert_eq!(thinker.context_window, Some(131_072));
    assert_eq!(openai_ladder(thinker), (vec![Low, Medium, High], true));
    assert!(!thinker.image_input);
    assert_eq!(thinker.tool_support, ToolSupport::Any);
    assert_eq!(thinker.max_output, None);

    let vision = row_of(&fetched, "llava:7b");
    assert_eq!(vision.context_window, Some(4_096));
    assert!(vision.image_input);
    // No `thinking` capability: no ladder and no `none` spelling, so the
    // request carries no `think` member at all.
    assert_eq!(openai_ladder(vision), (Vec::new(), false));

    let embedder = row_of(&fetched, "nomic-embed:1");
    assert_eq!(embedder.tool_support, ToolSupport::None);
    assert!(!embedder.image_input);

    // One tags request, then one probe per listed model, none authenticated.
    assert_eq!(seen.requests.len(), 4);
    let tags: Vec<_> = seen
        .requests
        .iter()
        .filter(|request| request.path == "/api/tags")
        .collect();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].method, "GET");
    let mut probed: Vec<_> = seen
        .requests
        .iter()
        .filter(|request| request.path == "/api/show")
        .map(|request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.header("content-type"), Some("application/json"));
            assert_eq!(request.header("accept"), Some("application/json"));
            request.body.clone()
        })
        .collect();
    probed.sort();
    assert_eq!(
        probed,
        [
            r#"{"model":"gpt-oss:20b"}"#,
            r#"{"model":"llava:7b"}"#,
            r#"{"model":"nomic-embed:1"}"#,
        ]
    );
    for request in &seen.requests {
        assert_eq!(request.header("authorization"), None, "{}", request.path);
    }
}

const CLOUD_TAGS: &str = r#"{"models":[{"name":"gpt-oss:120b"},{"model":"qwen3:32b","name":"Qwen 3 32B"},{"name":"deepseek-v4-flash"},{"name":"deepseek-v4.1-flash"},{"name":"glm-5.2"},{"name":"gemma3:27b"}]}"#;

const CLOUD_SHOWS: &[(&str, u16, &str)] = &[
    (
        "gpt-oss:120b",
        200,
        r#"{"capabilities":["completion","tools","thinking","vision"],"model_info":{"general.architecture":"gptoss","gptoss.context_length":262144}}"#,
    ),
    (
        "qwen3:32b",
        200,
        r#"{"capabilities":["completion","vision"]}"#,
    ),
    (
        "deepseek-v4-flash",
        200,
        r#"{"capabilities":["completion","tools","thinking"],"model_info":{"deepseek.context_length":1048576}}"#,
    ),
    (
        "deepseek-v4.1-flash",
        200,
        r#"{"capabilities":["completion","tools","thinking"],"model_info":{"deepseek.context_length":1048576}}"#,
    ),
    (
        "glm-5.2",
        200,
        r#"{"capabilities":["completion","tools","thinking"],"model_info":{"glm.context_length":1000000}}"#,
    ),
    (
        "gemma3:27b",
        200,
        r#"{"capabilities":["completion","tools","vision"],"model_info":{"gemma3.context_length":131072}}"#,
    ),
];

#[tokio::test]
async fn cloud_discovery_authenticates_every_probe_and_pins_the_output_cap() {
    let directory = TestDir::new();
    let script = serve(
        1 + CLOUD_SHOWS.len(),
        Duration::ZERO,
        ollama_route(CLOUD_TAGS, CLOUD_SHOWS),
    );
    let provider = ollama_provider("ollama-cloud", script.port, "", AuthStyle::Bearer);
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("cloud-key")),
    };
    let fetched = refresh(&provider, &credential, directory.path()).await;
    let seen = script.finish();
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );

    // The rows keep the server's order and every probe carried the key.
    assert_eq!(
        fetched
            .entries
            .iter()
            .map(|entry| entry.id.as_ref())
            .collect::<Vec<_>>(),
        [
            "gpt-oss:120b",
            "qwen3:32b",
            "deepseek-v4-flash",
            "deepseek-v4.1-flash",
            "glm-5.2",
            "gemma3:27b",
        ]
    );
    assert_eq!(seen.requests.len(), 7);
    for request in &seen.requests {
        assert_eq!(
            request.header("authorization"),
            Some("Bearer cloud-key"),
            "{}",
            request.path
        );
    }

    let gpt = row_of(&fetched, "gpt-oss:120b");
    assert_eq!(gpt.context_window, Some(262_144));
    assert!(gpt.image_input);
    assert_eq!(openai_ladder(gpt), (vec![Low, Medium, High], true));
    assert_eq!(gpt.max_output, None);

    let qwen = row_of(&fetched, "qwen3:32b");
    assert_eq!(qwen.display.as_ref(), "Qwen 3 32B");
    assert!(qwen.image_input);
    assert_eq!(qwen.tool_support, ToolSupport::None);
    assert_eq!(openai_ladder(qwen), (Vec::new(), false));

    // DeepSeek V4 Flash caps output at the cloud ceiling below its window;
    // the dated 4.1 id is not one of the two capped deployments.
    let flash = row_of(&fetched, "deepseek-v4-flash");
    assert_eq!(flash.context_window, Some(1_048_576));
    assert_eq!(flash.max_output, Some(65_536));
    assert_eq!(openai_ladder(flash), (vec![Low, High, Max], true));
    let newer = row_of(&fetched, "deepseek-v4.1-flash");
    assert_eq!(newer.max_output, None);
    assert_eq!(openai_ladder(newer), (vec![Low, High, Max], true));

    let glm = row_of(&fetched, "glm-5.2");
    assert_eq!(glm.context_window, Some(1_000_000));
    assert_eq!(openai_ladder(glm), (vec![High, Max], true));

    let gemma = row_of(&fetched, "gemma3:27b");
    assert!(gemma.image_input);
    assert_eq!(gemma.display.as_ref(), "gemma3:27b");
}

#[tokio::test]
async fn a_failed_probe_keeps_the_last_good_row_and_a_new_model_stays_tag_only() {
    let directory = TestDir::new();
    // First refresh: both probes answer; the rows land in the cache.
    let first = serve(
        3,
        Duration::ZERO,
        ollama_route(
            r#"{"models":[{"name":"seer"},{"name":"gone"}]}"#,
            &[
                (
                    "seer",
                    200,
                    r#"{"capabilities":["completion","tools","thinking","vision"],"model_info":{"llama.context_length":4096}}"#,
                ),
                ("gone", 200, r#"{"capabilities":["completion"]}"#),
            ],
        ),
    );
    let provider_first = ollama_provider("ollama", first.port, "", AuthStyle::None);
    let fetched = refresh(&provider_first, &Credential::None, directory.path()).await;
    first.finish();
    assert_eq!(fetched.source, CatalogSource::Live);

    // Second refresh: `seer` and the new `fresh` both fail their probe.
    let second = serve(
        3,
        Duration::ZERO,
        ollama_route(
            r#"{"models":[{"name":"seer"},{"name":"fresh"}]}"#,
            &[
                ("seer", 500, r#"{"error":"busy"}"#),
                ("fresh", 200, "<html>not json</html>"),
            ],
        ),
    );
    let provider_second = ollama_provider("ollama", second.port, "", AuthStyle::None);
    let fetched = refresh(&provider_second, &Credential::None, directory.path()).await;
    second.finish();
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(
        fetched
            .entries
            .iter()
            .map(|entry| entry.id.as_ref())
            .collect::<Vec<_>>(),
        ["seer", "fresh"]
    );
    // `seer` keeps what the last good refresh knew, thinking ladder included.
    let seer = row_of(&fetched, "seer");
    assert_eq!(seer.context_window, Some(4_096));
    assert!(seer.image_input);
    assert_eq!(openai_ladder(seer), (vec![Low, Medium, High, Max], true));
    // `fresh` has no history: exactly what `api/tags` alone says.
    let fresh = row_of(&fetched, "fresh");
    assert_eq!(fresh.context_window, None);
    assert!(!fresh.image_input);
    assert_eq!(fresh.tool_support, ToolSupport::Any);
}

#[tokio::test]
async fn a_server_without_a_show_endpoint_still_lists_its_models() {
    let directory = TestDir::new();
    let script = serve(
        3,
        Duration::ZERO,
        ollama_route(r#"{"models":[{"name":"a"},{"name":"b"}]}"#, &[]),
    );
    let provider = ollama_provider("ollama", script.port, "", AuthStyle::None);
    let fetched = refresh(&provider, &Credential::None, directory.path()).await;
    script.finish();
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(fetched.entries.len(), 2);
    for entry in &fetched.entries {
        assert_eq!(entry.context_window, None);
        assert!(!entry.image_input);
        assert_eq!(entry.tool_support, ToolSupport::Any);
    }
}

#[tokio::test]
async fn show_probes_run_in_bounded_parallel() {
    const MODELS: usize = 14;
    let directory = TestDir::new();
    let script = serve(1 + MODELS, Duration::from_millis(120), |request| {
        if request.path == "/api/tags" {
            let models: Vec<String> = (0..MODELS)
                .map(|index| format!(r#"{{"name":"model-{index}"}}"#))
                .collect();
            return (200, format!(r#"{{"models":[{}]}}"#, models.join(",")));
        }
        (200, String::from(r#"{"capabilities":["completion"]}"#))
    });
    let provider = ollama_provider("ollama", script.port, "", AuthStyle::None);
    let fetched = refresh(&provider, &Credential::None, directory.path()).await;
    let seen = script.finish();
    assert_eq!(fetched.entries.len(), MODELS);
    assert!(
        seen.max_in_flight > 1,
        "probes ran one at a time: {}",
        seen.max_in_flight
    );
    assert!(
        seen.max_in_flight <= 6,
        "probes exceeded the bound: {}",
        seen.max_in_flight
    );
}

#[tokio::test]
async fn a_failed_tag_list_falls_back_without_probing() {
    let directory = TestDir::new();
    let script = serve(1, Duration::ZERO, |_| (503, String::from("{}")));
    let provider = ollama_provider("ollama", script.port, "", AuthStyle::None);
    let fetched = refresh(&provider, &Credential::None, directory.path()).await;
    let seen = script.finish();
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::Status { status: 503, .. })
        ),
        "{:?}",
        fetched.live_error
    );
    assert_eq!(seen.requests.len(), 1);
}

/// Routes recorded replies: `tags` for `GET /api/tags`, then the recorded
/// `POST /api/show` reply of each listed model.
fn recorded_route(
    tags: String,
    shows: Vec<(&'static str, &'static str)>,
) -> impl Fn(&Captured) -> (u16, String) + Send + Sync + 'static {
    move |request| {
        if request.path == "/api/tags" {
            return (200, tags.clone());
        }
        let model = request.model();
        shows
            .iter()
            .find(|(id, _)| Some(*id) == model.as_deref())
            .map_or((404, String::from("{}")), |(_, body)| {
                (200, (*body).to_owned())
            })
    }
}

fn tag_list(ids: &[&str]) -> String {
    let rows: Vec<String> = ids
        .iter()
        .map(|id| format!(r#"{{"name":"{id}","model":"{id}"}}"#))
        .collect();
    format!(r#"{{"models":[{}]}}"#, rows.join(","))
}

#[tokio::test]
async fn recorded_cloud_replies_map_to_the_rows_the_server_describes() {
    use crate::family::ollama::{BOOLEAN_ON, live_fixtures as live};
    use dal_core::ThinkingLevel::{Minimal, Xhigh};

    let shows = vec![
        ("gpt-oss:120b", live::GPT_OSS),
        ("glm-5.2", live::GLM_5_2),
        ("deepseek-v4.1-flash", live::DEEPSEEK_V41),
        ("beam", live::BEAM),
        ("gemma4:31b", live::GEMMA_4),
        ("minimax-m2.7", live::MINIMAX_M2_7),
        ("mistral-large-4", live::MISTRAL_LARGE_4),
    ];
    let ids: Vec<&str> = shows.iter().map(|(id, _)| *id).collect();
    let directory = TestDir::new();
    let script = serve(
        1 + shows.len(),
        Duration::ZERO,
        recorded_route(tag_list(&ids), shows),
    );
    let provider = ollama_provider("ollama-cloud", script.port, "", AuthStyle::None);
    let fetched = refresh(&provider, &Credential::None, directory.path()).await;
    script.finish();
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );

    // The server's own `thinking.values` decide the ladder.
    let ladder = |id: &str| openai_ladder(row_of(&fetched, id));
    assert_eq!(ladder("gpt-oss:120b"), (vec![Low, Medium, High], false));
    assert_eq!(ladder("glm-5.2"), (vec![High, Max], true));
    assert_eq!(ladder("deepseek-v4.1-flash"), (vec![Low, High, Max], true));
    assert_eq!(ladder("beam"), (vec![Low, Medium, High, Xhigh, Max], false));
    // Reasoning without levels is a switch: the boolean level, `none` when
    // the server lists `false`, no `none` for an always-on model.
    assert_eq!(ladder("gemma4:31b"), (vec![BOOLEAN_ON], true));
    assert_eq!(ladder("minimax-m2.7"), (vec![BOOLEAN_ON], false));
    // A reasoning model whose server lists no values falls back to the
    // cloud ladder of its id.
    assert_eq!(
        ladder("mistral-large-4"),
        (vec![Minimal, Low, Medium, High], true)
    );

    assert_eq!(row_of(&fetched, "glm-5.2").context_window, Some(1_048_576));
    assert_eq!(
        row_of(&fetched, "gpt-oss:120b").context_window,
        Some(131_072)
    );
    assert!(row_of(&fetched, "gemma4:31b").image_input);
    assert!(row_of(&fetched, "deepseek-v4.1-flash").image_input);
    assert!(!row_of(&fetched, "gpt-oss:120b").image_input);
}

#[tokio::test]
async fn recorded_local_replies_map_a_thinking_model_and_a_plain_one() {
    use crate::family::ollama::{BOOLEAN_ON, live_fixtures as live};

    let shows = vec![
        ("qwen3:0.6b", live::LOCAL_QWEN3),
        ("gemma3:270m", live::LOCAL_GEMMA3),
    ];
    let directory = TestDir::new();
    let script = serve(
        3,
        Duration::ZERO,
        recorded_route(tag_list(&["qwen3:0.6b", "gemma3:270m"]), shows),
    );
    let provider = ollama_provider("ollama", script.port, "", AuthStyle::None);
    let fetched = refresh(&provider, &Credential::None, directory.path()).await;
    script.finish();
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );

    let qwen = row_of(&fetched, "qwen3:0.6b");
    assert_eq!(openai_ladder(qwen), (vec![BOOLEAN_ON], true));
    assert_eq!(qwen.context_window, Some(40_960));
    assert_eq!(qwen.tool_support, ToolSupport::Any);

    // No `thinking` and no `tools` capability: a thinking level clamps to off
    // and sends no `think` member (the server answers 400 to `think: true`),
    // and no tools are offered.
    let gemma = row_of(&fetched, "gemma3:270m");
    assert_eq!(openai_ladder(gemma), (Vec::new(), false));
    assert_eq!(gemma.context_window, Some(32_768));
    assert_eq!(gemma.tool_support, ToolSupport::None);
}
