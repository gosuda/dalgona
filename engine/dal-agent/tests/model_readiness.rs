//! Native host coverage for non-generating model readiness.
#![expect(
    clippy::expect_used,
    reason = "test fixtures abort at failed boundaries"
)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dal_agent::error::ServiceError;
use dal_agent::ext::{
    BoxFuture, Caller, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
    ModelRecord, ObserveHook, Services,
};
use dal_agent::{Env, Host, Product, SessionRef};
use dal_core::{
    Caps, ClientId, Config, ConfigProduct, ModelRequest, ModelRoute, Purpose, RequestParams,
    ServiceSet, SessionStart, ThinkingLevel, Workspace,
};
use tokio::sync::Notify;

type ReadinessResult = Result<Option<ModelRoute>, ServiceError>;
type CapturedServices = Arc<Mutex<Option<Arc<dyn Services>>>>;

#[derive(Clone)]
struct ReadinessProbe {
    route: ModelRoute,
    outcome: Arc<Mutex<Option<ReadinessResult>>>,
    capture_only: bool,
    completed: Arc<Notify>,
    captured_services: Option<CapturedServices>,
    captured_caller: Option<Arc<Mutex<Option<Caller>>>>,
}

impl ObserveHook<SessionStart> for ReadinessProbe {
    fn call(
        &self,
        _input: SessionStart,
        cx: dal_agent::ext::HookCx,
    ) -> BoxFuture<'static, Result<(), dal_agent::ext::HookError>> {
        let services = Arc::clone(&cx.services);
        let caller = cx.caller.clone();
        let route = self.route.clone();
        let outcome = Arc::clone(&self.outcome);
        let capture_only = self.capture_only;
        let completed = Arc::clone(&self.completed);
        let captured_services = self.captured_services.clone();
        let captured_caller = self.captured_caller.clone();
        Box::pin(async move {
            if let Some(slot) = captured_services {
                *slot.lock().expect("captured services lock") = Some(Arc::clone(&services));
            }
            if let Some(slot) = captured_caller {
                *slot.lock().expect("captured caller lock") = Some(caller.clone());
            }
            if capture_only {
                completed.notify_one();
                return Ok(());
            }
            let result = services.model_ready(&caller, route).await;
            *outcome.lock().expect("probe outcome lock") = Some(result);
            completed.notify_one();
            Ok(())
        })
    }
}

struct CountingModel {
    calls: Arc<AtomicUsize>,
}

impl ModelHandler for CountingModel {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let events = futures::stream::iter([
            Ok(dal_provider::StreamEvent::TextDelta {
                text: "unexpected synthetic generation".into(),
            }),
            Ok(dal_provider::StreamEvent::Stop {
                reason: dal_provider::StopReason::EndTurn,
            }),
        ]);
        Box::pin(std::future::ready(Ok(EventStream::new(events, || {}))))
    }
}

fn workspace(path: &Path) -> Workspace {
    Workspace::new(path.to_path_buf()).expect("workspace")
}

fn config(data: &Path, user: Option<&str>) -> Config {
    Config::load(ConfigProduct::Dalgon, data, "", user).expect("config")
}

fn env(cwd: PathBuf) -> Env {
    Env {
        vars: BTreeMap::<OsString, OsString>::new(),
        cwd,
        sandbox_helper: None,
    }
}

fn probe_extension(probe: ReadinessProbe) -> dal_agent::ext::Extension {
    ExtensionBuilder::new("readiness", "0.1.0", ServiceSet::default())
        .expect("extension builder")
        .on_session_start_lossless(probe)
        .build()
        .expect("extension")
}

fn request(route: ModelRoute) -> ModelRequest {
    ModelRequest {
        purpose: Purpose::Turn,
        model: route,
        system: "".into(),
        tools: Arc::from(Vec::new()),
        context: Arc::from(Vec::new()),
        params: RequestParams::default(),
        cache_key: None,
    }
}

fn api_route() -> ModelRoute {
    ModelRoute::Api {
        family: dal_core::Family::Chat,
        model: "gpt-4o".into(),
        provider: None,
    }
}

fn synthetic_caps() -> Caps {
    Caps {
        context_window: Some(100_000),
        thinking: Box::new([ThinkingLevel::Off]),
        tool_use: false,
        image_input: false,
        custom_grammar: false,
    }
}

fn scripted_step(text: &str) -> String {
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"{text}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":1,\"cached_input_tokens\":0,\"output_tokens\":1,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}"
    )
}

async fn open_ephemeral(host: &Host, root: &Path) -> dal_agent::Agent {
    host.open(
        SessionRef::Ephemeral {
            workspace: workspace(root),
        },
        ClientId::new("readiness-test"),
    )
    .await
    .expect("open session")
}

#[tokio::test]
async fn readiness_does_not_consume_scripted_step() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path().join("data");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&root).expect("workspace dir");
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, scripted_step("first")).expect("fixture");
    let user = format!(
        "[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let route = api_route();
    let outcome = Arc::new(Mutex::new(None));
    let completed = Arc::new(Notify::new());
    let captured_services = Arc::new(Mutex::new(None));
    let captured_caller = Arc::new(Mutex::new(None));
    let probe = ReadinessProbe {
        route: route.clone(),
        outcome: Arc::clone(&outcome),
        capture_only: false,
        completed: Arc::clone(&completed),
        captured_services: Some(Arc::clone(&captured_services)),
        captured_caller: Some(Arc::clone(&captured_caller)),
    };
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![probe_extension(probe)],
            bundled: Vec::new(),
        },
        config(&data, Some(&user)),
        env(root.clone()),
    )
    .await
    .expect("host");
    let agent = open_ephemeral(&host, &root).await;
    completed.notified().await;
    assert_eq!(
        *outcome.lock().expect("probe outcome lock"),
        Some(Ok(Some(route.clone())))
    );

    let services = captured_services
        .lock()
        .expect("captured services lock")
        .take()
        .expect("session services");
    let caller = captured_caller
        .lock()
        .expect("captured caller lock")
        .take()
        .expect("session caller");
    let inference = services
        .infer(&caller, request(route))
        .await
        .expect("first scripted inference");
    let text: String = inference
        .events
        .iter()
        .filter_map(|event| match event {
            dal_core::StreamEvent::Delta {
                channel: dal_core::StreamChannel::Text,
                text,
            } => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "first");
    host.close(agent.session()).await.expect("close session");
    host.shutdown(std::time::Duration::from_secs(1)).await;
}

#[tokio::test]
async fn synthetic_readiness_does_not_call_handler() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path().join("data");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&root).expect("workspace dir");
    let calls = Arc::new(AtomicUsize::new(0));
    let route = ModelRoute::synthetic("probe/model").expect("route");
    let outcome = Arc::new(Mutex::new(None));
    let completed = Arc::new(Notify::new());
    let probe = ReadinessProbe {
        route: route.clone(),
        outcome: Arc::clone(&outcome),
        capture_only: false,
        completed: Arc::clone(&completed),
        captured_services: None,
        captured_caller: None,
    };
    let extension = ExtensionBuilder::new("readiness", "0.1.0", ServiceSet::default())
        .expect("extension builder")
        .model(ModelRecord {
            id: dal_core::ModelId::parse("probe/model").expect("model id"),
            caps: synthetic_caps(),
            handler: Arc::new(CountingModel {
                calls: Arc::clone(&calls),
            }),
            export: None,
        })
        .on_session_start_lossless(probe)
        .build()
        .expect("extension");
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![extension],
            bundled: Vec::new(),
        },
        config(&data, None),
        env(root.clone()),
    )
    .await
    .expect("host");
    let agent = open_ephemeral(&host, &root).await;
    completed.notified().await;
    assert_eq!(
        *outcome.lock().expect("probe outcome lock"),
        Some(Ok(Some(route)))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    host.close(agent.session()).await.expect("close session");
    host.shutdown(std::time::Duration::from_secs(1)).await;
}

#[tokio::test]
async fn readiness_reports_missing_credentials_without_opening_transport() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path().join("data");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&root).expect("workspace dir");
    let route = ModelRoute::Api {
        family: dal_core::Family::Chat,
        model: "openai/gpt-4o".into(),
        provider: None,
    };
    let outcome = Arc::new(Mutex::new(None));
    let completed = Arc::new(Notify::new());
    let user = "[providers.openai]\napi = \"openai_chat\"\nbase_url = \"http://127.0.0.1:1/v1\"\n";
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![probe_extension(ReadinessProbe {
                route: route.clone(),
                outcome: Arc::clone(&outcome),
                capture_only: false,
                completed: Arc::clone(&completed),
                captured_services: None,
                captured_caller: None,
            })],
            bundled: Vec::new(),
        },
        config(&data, Some(user)),
        env(root.clone()),
    )
    .await
    .expect("host");
    let agent = open_ephemeral(&host, &root).await;
    completed.notified().await;
    assert_eq!(*outcome.lock().expect("probe outcome lock"), Some(Ok(None)));
    host.close(agent.session()).await.expect("close session");
    host.shutdown(std::time::Duration::from_secs(1)).await;
}

#[tokio::test]
async fn readiness_preserves_cancellation() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path().join("data");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&root).expect("workspace dir");
    let route = ModelRoute::synthetic("missing/model").expect("route");
    let outcome = Arc::new(Mutex::new(None));
    let completed = Arc::new(Notify::new());
    let captured_services = Arc::new(Mutex::new(None));
    let captured_caller = Arc::new(Mutex::new(None));
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![probe_extension(ReadinessProbe {
                route: route.clone(),
                outcome,
                capture_only: true,
                completed: Arc::clone(&completed),
                captured_services: Some(Arc::clone(&captured_services)),
                captured_caller: Some(Arc::clone(&captured_caller)),
            })],
            bundled: Vec::new(),
        },
        config(&data, None),
        env(root.clone()),
    )
    .await
    .expect("host");
    let agent = open_ephemeral(&host, &root).await;
    completed.notified().await;
    let services = captured_services
        .lock()
        .expect("captured services lock")
        .take()
        .expect("session services");
    let caller = captured_caller
        .lock()
        .expect("captured caller lock")
        .take()
        .expect("session caller");
    host.close(agent.session()).await.expect("close session");
    let result = services.model_ready(&caller, route).await;
    assert_eq!(result, Err(ServiceError::Cancelled));
    host.shutdown(std::time::Duration::from_secs(1)).await;
}

#[tokio::test]
async fn readiness_preserves_provider_alias_identity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path().join("data");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&root).expect("workspace dir");
    let auth = data.join("auth.json");
    std::fs::write(
        &auth,
        r#"{"openai":{"kind":"api_key","key":"readiness-test-key"}}"#,
    )
    .expect("auth file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600))
            .expect("auth permissions");
    }
    let user = "[aliases]\nchosen = \"openai-chat/gpt-4o\"\n[providers.openai]\napi = \"openai_chat\"\nbase_url = \"http://127.0.0.1:1/v1\"\n";
    let route = ModelRoute::from_id("chosen");
    let outcome = Arc::new(Mutex::new(None));
    let completed = Arc::new(Notify::new());
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![probe_extension(ReadinessProbe {
                route: route.clone(),
                outcome: Arc::clone(&outcome),
                capture_only: false,
                completed: Arc::clone(&completed),
                captured_services: None,
                captured_caller: None,
            })],
            bundled: Vec::new(),
        },
        config(&data, Some(user)),
        env(root.clone()),
    )
    .await
    .expect("host");
    let agent = open_ephemeral(&host, &root).await;
    completed.notified().await;
    assert_eq!(
        *outcome.lock().expect("probe outcome lock"),
        Some(Ok(Some(route)))
    );
    host.close(agent.session()).await.expect("close session");
    host.shutdown(std::time::Duration::from_secs(1)).await;
}

async fn held_catalog_request(
    listener: tokio::net::TcpListener,
    started: tokio::sync::oneshot::Sender<()>,
    released: tokio::sync::oneshot::Receiver<()>,
) -> bool {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let (socket, _) = listener.accept().await.expect("catalog request");
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(
            reader.read_line(&mut line).await.expect("request header"),
            0
        );
        if line == "\r\n" {
            break;
        }
    }
    started.send(()).expect("request entry signal");
    let mut byte = [0_u8; 1];
    tokio::select! {
        result = reader.read(&mut byte) => {
            assert_eq!(result.expect("request closure"), 0);
            true
        }
        _ = released => {
            let body = r#"{"data":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = reader.get_mut().write_all(response.as_bytes()).await;
            false
        }
    }
}

#[tokio::test]
async fn closing_session_drops_an_in_flight_readiness_request() {
    use tokio::sync::oneshot;

    let temp = tempfile::tempdir().expect("tempdir");
    let data = temp.path().join("data");
    let root = temp.path().join("workspace");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&root).expect("workspace dir");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("catalog listener");
    let address = listener.local_addr().expect("catalog address");
    let (started, request_started) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(held_catalog_request(listener, started, released));
    let outcome = Arc::new(Mutex::new(None));
    let user =
        format!("[providers.openai]\napi = \"openai_chat\"\nbase_url = \"http://{address}/v1\"\n");
    let mut environment = env(root.clone());
    environment
        .vars
        .insert("OPENAI_API_KEY".into(), "readiness-test-key".into());
    let host = Host::start(
        Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![probe_extension(ReadinessProbe {
                route: api_route(),
                outcome,
                capture_only: false,
                completed: Arc::new(Notify::new()),
                captured_services: None,
                captured_caller: None,
            })],
            bundled: Vec::new(),
        },
        config(&data, Some(&user)),
        environment,
    )
    .await
    .expect("host");
    let agent = open_ephemeral(&host, &root).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), request_started)
        .await
        .expect("readiness reaches catalog")
        .expect("request entry");
    let closed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        host.close(agent.session()),
    )
    .await;
    let dropped = tokio::time::timeout(std::time::Duration::from_secs(1), server.join_next()).await;
    let dropped_before_release = if let Ok(result) = dropped {
        result
            .expect("catalog server task")
            .expect("catalog server")
    } else {
        let _ = release.send(());
        server
            .join_next()
            .await
            .expect("catalog server task")
            .expect("release held catalog")
    };
    host.shutdown(std::time::Duration::from_secs(1)).await;
    closed
        .expect("session close finishes")
        .expect("session closes");
    assert!(
        dropped_before_release,
        "the readiness request outlived its closed session"
    );
}
