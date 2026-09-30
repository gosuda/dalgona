#![expect(clippy::unwrap_used, reason = "SC test")]
#![expect(clippy::expect_used, reason = "SC test")]

use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    sync::{Arc, Mutex},
    time::Duration,
};

use dal_agent::{
    Env, Host, Product,
    ext::{
        BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
        ScopeError,
    },
};
use dal_core::{
    AgentStart, Budget, CallId, Caps, Config, ConfigProduct, DenyReason, Family, ListQuery,
    ModelId, ModelRequest, ModelRoute, OnError, ScopeSpec, ServiceSet, ThinkingLevel, Workspace,
};
use dal_wire::router::RouterOptions;
use sonic_rs::{JsonValueTrait, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SCRIPTED_TEXT: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"Fused\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":4,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

struct Fusion {
    log: Arc<Mutex<Vec<String>>>,
}

impl ModelHandler for Fusion {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            self.log.lock().unwrap().push("handler ran".to_owned());
            let spec = ScopeSpec {
                limit: 1,
                on_error: OnError::Settle,
                budget: Budget::default(),
            };
            let scope = cx.scope(spec).expect("scope opens");
            let start = AgentStart {
                call: CallId::new("member"),
                name: "member".into(),
                prompt: "work".into(),
                model: None,
                role: None,
                system: None,
                tools: None,
                workspace: None,
            };
            let member = scope.agent(start).expect("member handle admitted");
            let denied = member.result().await.err();
            self.log.lock().unwrap().push(format!("member {denied:?}"));
            let mut request = request;
            request.model = ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-6-luna".into(),
            };
            cx.forward(request, &[]).await
        })
    }
}

async fn post(addr: std::net::SocketAddr, path: &str, body: &str) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let request = format!(
        "POST {path} HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut bytes))
        .await
        .expect("response in time")
        .expect("read");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let (head, rest) = text.split_once("\r\n\r\n").expect("response head");
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("status");
    let chunked = head
        .lines()
        .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"));
    if !chunked {
        return (status, rest.to_owned());
    }
    let mut body = String::new();
    let mut remaining = rest;
    while let Some((size, tail)) = remaining.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).expect("chunk size");
        if size == 0 {
            break;
        }
        body.push_str(&tail[..size]);
        remaining = &tail[size + 2..];
    }
    (status, body)
}

#[tokio::test]
async fn synthetic_model_runs_through_router_without_session_journal()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let tmp = tempfile::tempdir()?;
    let data = tmp.path().join("data");
    let workspace = tmp.path().join("w");
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&workspace)?;
    let fixture = data.join("script.jsonl");
    std::fs::write(&fixture, SCRIPTED_TEXT)?;
    let user = format!(
        "[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))?;
    let log = Arc::new(Mutex::new(Vec::new()));
    let extension = ExtensionBuilder::new("fusion", "0.1.0", ServiceSet::default())?
        .model(ModelRecord {
            id: ModelId::parse("dalgona/fusion")?,
            caps: Caps {
                context_window: Some(200_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Fusion {
                log: Arc::clone(&log),
            }),
            export: None,
        })
        .build()?;
    let product = Product {
        name: "dal",
        data_root: data.clone(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let options = RouterOptions {
        bind: "127.0.0.1".to_owned(),
        port: 0,
        public: false,
        a2a: false,
        token_file: data.join("serve.token"),
        approval: dal_core::ApprovalMode::Ask,
        origins: Vec::new(),
        aliases: BTreeMap::new(),
        workspace: workspace.clone(),
    };
    let stop = tokio_util::sync::CancellationToken::new();
    let handle = dal_wire::serve_router(host.clone(), options, stop.clone()).await?;
    let addr = handle.local_addr();
    let body = r#"{"model":"dalgona/fusion","messages":[{"role":"user","content":"hi"}]}"#;
    let driven = async {
        let reply = post(addr, "/v1/chat/completions", body).await;
        stop.cancel();
        reply
    };
    let (waited, (status, text)) = tokio::join!(handle.wait(), driven);
    waited?;
    assert_eq!(status, 200, "{text}");
    let json: Value = sonic_rs::from_str(&text)?;
    assert_eq!(
        json["choices"][0]["message"]["content"].as_str(),
        Some("Fused"),
        "{text}"
    );
    assert_eq!(json["usage"]["prompt_tokens"].as_u64(), Some(10), "{text}");
    let expected = ScopeError::Denied(DenyReason::Unavailable {
        what: "member sessions outside a session".into(),
    });
    let lines = log.lock().unwrap().clone();
    assert_eq!(
        lines,
        vec![
            "handler ran".to_owned(),
            format!("member {:?}", Some(expected))
        ]
    );
    let store = dal_store::Store::new(data, Workspace::new(workspace)?, dal_core::Product::Dal);
    assert!(
        store
            .list(ListQuery {
                limit: None,
                cursor: None,
                search: None,
            })?
            .items
            .is_empty(),
        "the relay journals nothing"
    );
    let _ = host.shutdown(Duration::from_secs(1)).await;
    Ok(())
}
