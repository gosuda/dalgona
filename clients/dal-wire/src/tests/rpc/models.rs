//! `models/list` and `auth/status` over the real RPC loop and the real remote
//! client, against a real `Host` whose catalog comes from loopback providers.
//!
//! A row names the configured provider that produced it: `route` is the typed
//! selection and `reference` its canonical `provider/model` name, while `id`,
//! `provider` (the family label), `name` and `contextWindow` keep the shape
//! older clients read.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::sync::Arc;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Env, Host, Product};
use dal_core::{
    Caps, Config, ConfigProduct, ModelId, ModelRequest, ModelRoute, ServiceSet, ThinkingLevel,
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::super::support::{Rig, Rpc, initialize, result};
use super::with_rpc;

/// The model list each loopback provider serves. `gpt-4o` is on all three
/// providers. The `openrouter` list also holds a native id that starts with
/// its own provider name (`openrouter/free`), one that spells another
/// provider's `provider/model` text (`openai/gpt-4o`), and a bare `free`.
fn listing(path: &str) -> Option<&'static str> {
    match path {
        "/openai/v1/models" => Some(r#"{"data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]}"#),
        "/openrouter/api/v1/models" => Some(
            r#"{"data":[{"id":"gpt-4o"},{"id":"openai/gpt-4o"},{"id":"free"},{"id":"openrouter/free"},{"id":"anthropic/claude-3"}]}"#,
        ),
        "/acme/v1/models" => Some(r#"{"data":[{"id":"gpt-4o"}]}"#),
        _ => None,
    }
}

/// A cleartext model-list server on `127.0.0.1`.
struct Loopback {
    address: SocketAddr,
    _server: JoinSet<()>,
}

impl Loopback {
    async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("loopback listener");
        let address = listener.local_addr().expect("loopback address");
        let mut server = JoinSet::new();
        server.spawn(accept(listener));
        Self {
            address,
            _server: server,
        }
    }
}

async fn accept(listener: TcpListener) {
    let mut connections = JoinSet::new();
    while let Ok((socket, _)) = listener.accept().await {
        connections.spawn(serve(socket));
    }
}

async fn serve(socket: TcpStream) {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
        return;
    }
    let target = line.split_whitespace().nth(1).unwrap_or_default();
    let path = target.split('?').next().unwrap_or_default().to_owned();
    loop {
        line.clear();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" {
            break;
        }
    }
    let (status, body) = match listing(&path) {
        Some(body) => ("200 OK", body),
        None => ("404 Not Found", ""),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = reader.get_mut().write_all(response.as_bytes()).await;
    let _ = reader.get_mut().shutdown().await;
}

/// Fails a turn without touching the network; one test drives a prompt so
/// the journal exists before its records are asserted.
struct NeverInfer;

impl ModelHandler for NeverInfer {
    fn run<'a>(
        &'a self,
        _request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async { Err(ModelError::PrivateRounds) })
    }
}

fn synthetic(id: &str) -> ModelRecord {
    ModelRecord {
        id: ModelId::parse(id).expect("synthetic model id"),
        caps: Caps {
            context_window: None,
            thinking: Box::new([ThinkingLevel::Off]),
            tool_use: false,
            image_input: false,
            custom_grammar: false,
        },
        handler: Arc::new(NeverInfer),
        export: None,
    }
}

/// Starts a real host whose catalog comes from the loopback providers:
/// `openai` and `openrouter` (both Responses family, both listing `gpt-4o`),
/// a custom Chat provider `acme-proxy` whose id is not its family label
/// (also listing `gpt-4o`), and two synthetic models. `openrouter/free` is a
/// synthetic id and also the native id `openrouter` lists.
async fn catalog_rig(server: &Loopback) -> Rig {
    let dir = tempfile::tempdir().expect("temporary root");
    let data = dir.path().join("data");
    let workspace = dir.path().join("w");
    std::fs::create_dir_all(&data).expect("data root");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let address = server.address;
    let user = format!(
        r#"model = "openai/gpt-4o"

[retry]
request_max_retries = 0
stream_max_retries = 0

[providers.openai]
base_url = "http://{address}/openai/v1"

[providers.openrouter]
base_url = "http://{address}/openrouter/api/v1"
key_env = "OPENAI_API_KEY"

[providers.acme-proxy]
api = "openai_chat"
base_url = "http://{address}/acme/v1"
key_env = "OPENAI_API_KEY"
"#
    );
    let config =
        Config::load(ConfigProduct::Dalgon, &data, "", Some(&user)).expect("test config loads");
    let models = ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::EMPTY)
        .expect("extension name")
        .model(synthetic("acme/fast"))
        .model(synthetic("openrouter/free"))
        .build()
        .expect("fixture extension builds");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![models],
        bundled: Vec::new(),
    };
    let mut vars = BTreeMap::new();
    vars.insert(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"));
    let env = Env {
        vars,
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env)
        .await
        .expect("host starts");
    Rig {
        dir,
        host,
        workspace,
        gate: Arc::new(Semaphore::new(0)),
        _tasks: JoinSet::new(),
    }
}

/// Reads the `models` rows of one `models/list` reply.
async fn model_rows(rpc: &mut Rpc, id: i64) -> Vec<Value> {
    let reply = rpc.call(id, "models/list", sonic_rs::json!({})).await;
    result(&reply)["models"]
        .as_array()
        .expect("model rows")
        .iter()
        .cloned()
        .collect()
}

/// What tells one `models/list` row apart: its legacy `id` and family label,
/// the kind and provider of its typed route, and its reference.
#[derive(Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Summary {
    id: String,
    label: String,
    kind: String,
    provider: Option<String>,
    reference: Option<String>,
}

fn summarize(row: &Value) -> Summary {
    Summary {
        id: row["id"].as_str().expect("id").to_owned(),
        label: row["provider"].as_str().expect("family label").to_owned(),
        kind: row["route"]["kind"]
            .as_str()
            .expect("route kind")
            .to_owned(),
        provider: row["route"]["provider"].as_str().map(str::to_owned),
        reference: row["reference"].as_str().map(str::to_owned),
    }
}

fn summary(id: &str, label: &str, kind: &str, provider: Option<&str>, reference: &str) -> Summary {
    Summary {
        id: id.to_owned(),
        label: label.to_owned(),
        kind: kind.to_owned(),
        provider: provider.map(str::to_owned),
        reference: Some(reference.to_owned()),
    }
}

/// Decodes the typed route of one row the way a client does.
fn route_of(row: &Value) -> ModelRoute {
    let text = sonic_rs::to_string(&row["route"]).expect("route json");
    sonic_rs::from_str(&text).expect("a typed route")
}

#[tokio::test]
async fn models_list_names_every_row_by_its_producing_provider() {
    let server = Loopback::start().await;
    let rig = catalog_rig(&server).await;
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let rows = model_rows(&mut rpc, 1).await;
        let mut seen: Vec<Summary> = rows.iter().map(summarize).collect();
        seen.sort();
        // `id` and the family label keep their original bytes: the three
        // `gpt-4o` rows share them, and so do `openrouter`'s `free` and
        // `openrouter/free` rows with the other `openai`-labelled rows. The
        // route provider and the reference are what tell the rows apart.
        let mut wanted = vec![
            summary("gpt-4o", "openai", "api", Some("openai"), "openai/gpt-4o"),
            summary(
                "gpt-4o-mini",
                "openai",
                "api",
                Some("openai"),
                "openai/gpt-4o-mini",
            ),
            summary(
                "gpt-4o",
                "openai",
                "api",
                Some("openrouter"),
                "openrouter/gpt-4o",
            ),
            summary(
                "openai/gpt-4o",
                "openai",
                "api",
                Some("openrouter"),
                "openrouter/openai/gpt-4o",
            ),
            summary(
                "openrouter/free",
                "openai",
                "api",
                Some("openrouter"),
                "openrouter/openrouter/free",
            ),
            summary(
                "anthropic/claude-3",
                "openai",
                "api",
                Some("openrouter"),
                "openrouter/anthropic/claude-3",
            ),
            summary(
                "free",
                "openai",
                "api",
                Some("openrouter"),
                "openrouter/free",
            ),
            summary(
                "gpt-4o",
                "openai",
                "api",
                Some("acme-proxy"),
                "acme-proxy/gpt-4o",
            ),
            summary("acme/fast", "acme", "synthetic", None, "acme/fast"),
            summary(
                "openrouter/free",
                "openrouter",
                "synthetic",
                None,
                "openrouter/free",
            ),
        ];
        wanted.sort();
        assert_eq!(seen, wanted);
        let order: Vec<&str> = rows
            .iter()
            .map(|row| row["id"].as_str().expect("id"))
            .collect();
        assert!(order.is_sorted(), "rows stay sorted by id: {order:?}");
    })
    .await;
}

#[tokio::test]
async fn a_row_keeps_the_legacy_members_of_its_producer_and_adds_only_route_and_reference() {
    let server = Loopback::start().await;
    let rig = catalog_rig(&server).await;
    let infos = rig.host.models(None).await.expect("host models");
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let rows = model_rows(&mut rpc, 1).await;
        assert_eq!(rows.len(), infos.len(), "one row per listed model");
        for row in &rows {
            let route = route_of(row);
            let info = infos
                .iter()
                .find(|info| info.route == route)
                .unwrap_or_else(|| panic!("no listed model carries {route:?}"));
            assert_eq!(row["name"].as_str(), Some(&*info.name), "{route:?}");
            assert_eq!(
                row["contextWindow"].as_u64(),
                info.caps.context_window.map(u64::from),
                "{route:?}"
            );
            let mut members: Vec<&str> = row
                .as_object()
                .expect("row object")
                .iter()
                .map(|(name, _)| name)
                .collect();
            members.sort_unstable();
            let mut wanted = vec!["id", "name", "provider", "reference", "route"];
            if info.caps.context_window.is_some() {
                wanted.push("contextWindow");
            }
            wanted.sort_unstable();
            assert_eq!(members, wanted, "{route:?}");
        }
        // Every API name still ends in the producer's own `provider/native`
        // text; that suffix is what the picker label is built from.
        let named = rows.iter().find(|row| {
            row["route"]["provider"].as_str() == Some("acme-proxy")
                && row["id"].as_str() == Some("gpt-4o")
        });
        let name = named.and_then(|row| row["name"].as_str()).expect("a name");
        assert!(name.ends_with(" · acme-proxy/gpt-4o"), "{name}");
    })
    .await;
}

#[tokio::test]
async fn same_family_providers_listing_one_model_share_legacy_bytes_and_differ_in_route() {
    let server = Loopback::start().await;
    let rig = catalog_rig(&server).await;
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let rows = model_rows(&mut rpc, 1).await;
        let mut twins: Vec<&Value> = rows
            .iter()
            .filter(|row| row["id"].as_str() == Some("gpt-4o"))
            .collect();
        assert_eq!(twins.len(), 3, "one `gpt-4o` row per provider");
        twins.sort_by_key(|row| row["route"]["provider"].as_str().map(str::to_owned));
        let identity: Vec<(&str, &str, &str, &str)> = twins
            .iter()
            .map(|row| {
                (
                    row["id"].as_str().expect("id"),
                    row["provider"].as_str().expect("family label"),
                    row["route"]["family"].as_str().expect("route family"),
                    row["route"]["provider"].as_str().expect("route provider"),
                )
            })
            .collect();
        // The legacy pair `(id, provider)` is identical for all three; the
        // two Responses providers differ only by the route's provider.
        assert_eq!(
            identity,
            [
                ("gpt-4o", "openai", "openai_chat", "acme-proxy"),
                ("gpt-4o", "openai", "openai_responses", "openai"),
                ("gpt-4o", "openai", "openai_responses", "openrouter"),
            ]
        );
        let references: Vec<&str> = twins
            .iter()
            .map(|row| row["reference"].as_str().expect("reference"))
            .collect();
        assert_eq!(
            references,
            ["acme-proxy/gpt-4o", "openai/gpt-4o", "openrouter/gpt-4o"]
        );
        let names: Vec<&str> = twins
            .iter()
            .map(|row| row["name"].as_str().expect("name"))
            .collect();
        for (name, reference) in names.iter().zip(&references) {
            assert!(
                name.ends_with(&format!(" · {reference}")),
                "{name} names {reference}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn an_api_native_id_and_a_synthetic_id_with_one_spelling_stay_two_rows() {
    let server = Loopback::start().await;
    let rig = catalog_rig(&server).await;
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let rows = model_rows(&mut rpc, 1).await;
        let spelled: Vec<&Value> = rows
            .iter()
            .filter(|row| row["id"].as_str() == Some("openrouter/free"))
            .collect();
        assert_eq!(spelled.len(), 2, "the API native and the synthetic model");
        let api_row = spelled
            .iter()
            .find(|row| row["route"]["kind"].as_str() == Some("api"))
            .expect("the API row");
        let synthetic_row = spelled
            .iter()
            .find(|row| row["route"]["kind"].as_str() == Some("synthetic"))
            .expect("the synthetic row");
        let api_route = &api_row["route"];
        assert_eq!(api_route["family"].as_str(), Some("openai_responses"));
        assert_eq!(api_route["model"].as_str(), Some("openrouter/free"));
        assert_eq!(api_route["provider"].as_str(), Some("openrouter"));
        assert_eq!(api_row["provider"].as_str(), Some("openai"));
        assert_eq!(
            api_row["reference"].as_str(),
            Some("openrouter/openrouter/free")
        );
        let synthetic_route = &synthetic_row["route"];
        assert_eq!(synthetic_route["id"].as_str(), Some("openrouter/free"));
        assert!(synthetic_route["provider"].is_null(), "no provider");
        assert_eq!(synthetic_row["provider"].as_str(), Some("openrouter"));
        assert_eq!(synthetic_row["reference"].as_str(), Some("openrouter/free"));
        assert_ne!(route_of(api_row), route_of(synthetic_row));
        // The bare native `free` on `openrouter` is spelled `openrouter/free`
        // by its reference too; its route, not the text, says which row it is.
        let shadow: Vec<(&str, Option<&str>)> = rows
            .iter()
            .filter(|row| row["reference"].as_str() == Some("openrouter/free"))
            .map(|row| {
                (
                    row["route"]["kind"].as_str().expect("route kind"),
                    row["route"]["model"].as_str(),
                )
            })
            .collect();
        assert_eq!(shadow.len(), 2);
        assert!(shadow.contains(&("api", Some("free"))), "{shadow:?}");
        assert!(shadow.contains(&("synthetic", None)), "{shadow:?}");
    })
    .await;
}

#[tokio::test]
async fn auth_status_keeps_its_family_labelled_rows() {
    let server = Loopback::start().await;
    let rig = catalog_rig(&server).await;
    with_rpc(&rig, |mut rpc| async move {
        initialize(&mut rpc).await;
        let status = rpc.call(1, "auth/status", sonic_rs::json!({})).await;
        let rows = result(&status)["providers"].as_array().expect("rows");
        let names: Vec<&str> = rows
            .iter()
            .map(|row| row["provider"].as_str().expect("provider"))
            .collect();
        // Sorted by id, each provider once.
        assert!(names.is_sorted(), "{names:?}");
        let mut unique = names.clone();
        unique.dedup();
        assert_eq!(unique, names, "one row per provider");
        // The family label of every chat or responses model is `openai`, and
        // a synthetic model contributes its namespace.
        for label in ["openai", "openrouter", "acme"] {
            assert!(names.contains(&label), "{label} in {names:?}");
        }
        // A custom provider's configured id is not an `auth/status` row: the
        // catalog contributes family labels, exactly as before.
        assert!(!names.contains(&"acme-proxy"), "{names:?}");
        for row in rows {
            let mut members: Vec<&str> = row
                .as_object()
                .expect("row object")
                .iter()
                .map(|(name, _)| name)
                .collect();
            members.sort_unstable();
            assert_eq!(members, ["provider", "state"], "{row}");
        }
        let state_of = |provider: &str| {
            rows.iter()
                .find(|row| row["provider"].as_str() == Some(provider))
                .and_then(|row| row["state"].as_str())
        };
        assert_eq!(state_of("openai"), Some("ready"));
        assert_eq!(state_of("acme"), Some("not_configured"));
    })
    .await;
}

#[cfg(unix)]
mod remote {
    use std::path::Path;
    use std::time::Duration;

    use dal_agent::SessionRef;
    use dal_core::{Command, Expect, Family, ModelRoute, PageReq, Part, Save};
    use sonic_rs::{JsonValueTrait, Value};

    use super::super::super::support::{Rig, find_dirs};
    use super::{Loopback, catalog_rig};
    use crate::error::WireError;
    use crate::remote::{RemoteDelivery, RemoteEndpoint, RemoteHost, RemoteModel};
    use std::num::NonZeroU64;

    /// Serves `rig`'s host on a private local socket and runs `client`
    /// against a connected [`RemoteHost`] until it finishes.
    async fn with_remote<F>(rig: &Rig, client: impl FnOnce(RemoteHost) -> F)
    where
        F: Future<Output = ()>,
    {
        let socket_dir = rig.dir.path().join("sock");
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&socket_dir)
                .expect("private socket directory");
        }
        let socket = socket_dir.join("rpc.sock");
        let serve_host = rig.host.clone();
        let listening = crate::serve_local(&socket, None, None, move |transport| {
            let host = serve_host.clone();
            Box::pin(crate::serve_rpc(host, transport))
        });
        let connected = async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while !socket.exists() {
                assert!(tokio::time::Instant::now() < deadline, "socket appears");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let host = RemoteHost::connect(RemoteEndpoint::LocalSocket(socket.clone()))
                .await
                .expect("connect");
            client(host).await;
        };
        tokio::select! {
            outcome = listening => panic!("the local server ended: {outcome:?}"),
            outcome = tokio::time::timeout(Duration::from_secs(120), connected) => {
                outcome.expect("the remote client finishes in time");
            }
        }
    }

    fn api(family: Family, model: &str, provider: &str) -> ModelRoute {
        ModelRoute::Api {
            family,
            model: model.into(),
            provider: Some(provider.into()),
        }
    }

    /// The typed routes of the rows named `reference`.
    fn routes_named<'a>(
        models: &'a [RemoteModel],
        reference: &'a str,
    ) -> impl Iterator<Item = ModelRoute> + 'a {
        models
            .iter()
            .filter(move |model| model.reference.as_deref() == Some(reference))
            .filter_map(|model| model.route.clone())
    }

    /// The API route of the row named `reference`.
    fn api_named(models: &[RemoteModel], reference: &str) -> ModelRoute {
        routes_named(models, reference)
            .find(|route| matches!(route, ModelRoute::Api { .. }))
            .unwrap_or_else(|| panic!("no API row {reference}"))
    }

    /// The synthetic route of the row named `reference`.
    fn synthetic_named(models: &[RemoteModel], reference: &str) -> ModelRoute {
        routes_named(models, reference)
            .find(|route| matches!(route, ModelRoute::Synthetic { .. }))
            .unwrap_or_else(|| panic!("no synthetic row {reference}"))
    }

    #[tokio::test]
    async fn the_remote_client_decodes_the_routes_and_references_of_the_real_server() {
        let server = Loopback::start().await;
        let rig = catalog_rig(&server).await;
        with_remote(&rig, |host| async move {
            let models = host.models().await.expect("models decode");
            assert_eq!(models.len(), 10);
            for model in &models {
                let route = model.route.as_ref().expect("a current server types a row");
                let reference = model
                    .reference
                    .as_deref()
                    .expect("a current server names a row");
                match route {
                    ModelRoute::Api {
                        model: native,
                        provider: Some(provider),
                        ..
                    } => assert_eq!(reference, format!("{provider}/{native}")),
                    ModelRoute::Api { provider: None, .. } => {
                        panic!("an API row without its provider: {model:?}")
                    }
                    ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => {
                        assert_eq!(reference, &**id);
                    }
                }
            }
            // The three `gpt-4o` rows decode to three distinct routes.
            let mut gpt4o: Vec<ModelRoute> = models
                .iter()
                .filter(|model| model.id == "gpt-4o")
                .filter_map(|model| model.route.clone())
                .collect();
            gpt4o.sort_by_key(|route| route.provider().map(str::to_owned));
            assert_eq!(
                gpt4o,
                [
                    api(Family::Chat, "gpt-4o", "acme-proxy"),
                    api(Family::Responses, "gpt-4o", "openai"),
                    api(Family::Responses, "gpt-4o", "openrouter"),
                ]
            );
            // The self-prefixed native keeps its slash and its provider; the
            // synthetic model with the same id is a different route.
            assert_eq!(
                api_named(&models, "openrouter/openrouter/free"),
                api(Family::Responses, "openrouter/free", "openrouter")
            );
            assert_eq!(
                synthetic_named(&models, "openrouter/free"),
                ModelRoute::Synthetic {
                    id: "openrouter/free".into(),
                }
            );
            assert_eq!(
                api_named(&models, "openrouter/free"),
                api(Family::Responses, "free", "openrouter")
            );
            // The legacy members decode as before.
            let acme = models
                .iter()
                .find(|model| model.reference.as_deref() == Some("acme-proxy/gpt-4o"))
                .expect("the custom provider row");
            assert_eq!(acme.id, "gpt-4o");
            assert_eq!(acme.provider, "openai");
            assert!(acme.name.ends_with(" · acme-proxy/gpt-4o"), "{}", acme.name);
        })
        .await;
    }

    /// One `model` journal record as `(api family or kind, native or
    /// synthetic id, provider)`.
    type Journaled = (String, String, Option<String>);

    fn journal_models(data: &Path, session: &str) -> Vec<Journaled> {
        // The journal is created with the first record; until then there is
        // nothing to read.
        let Some(journal) = find_dirs(data, session)
            .into_iter()
            .map(|dir| dir.join("journal.jsonl"))
            .find(|journal| journal.is_file())
        else {
            return Vec::new();
        };
        let text = std::fs::read_to_string(journal).unwrap_or_default();
        text.lines()
            .filter_map(|line| sonic_rs::from_str::<Value>(line).ok())
            .filter(|record| record["type"].as_str() == Some("model"))
            .map(|record| match record["route"]["synthetic"]["id"].as_str() {
                Some(id) => ("synthetic".to_owned(), id.to_owned(), None),
                None => (
                    record["api"].as_str().expect("api family").to_owned(),
                    record["model"].as_str().expect("native id").to_owned(),
                    record["provider"].as_str().map(str::to_owned),
                ),
            })
            .collect()
    }

    fn journaled(family: &str, model: &str, provider: Option<&str>) -> Journaled {
        (
            family.to_owned(),
            model.to_owned(),
            provider.map(str::to_owned),
        )
    }

    #[tokio::test]
    async fn a_remote_selection_of_every_row_reaches_the_journal_with_its_provider() {
        let server = Loopback::start().await;
        let rig = catalog_rig(&server).await;
        let workspace = rig.core_workspace();
        let data = rig.data();
        with_remote(&rig, |host| async move {
            let models = host.models().await.expect("models decode");
            let agent = host
                .open(SessionRef::New {
                    workspace,
                    name: None,
                })
                .await
                .expect("open a session");
            // Both Responses providers list `gpt-4o`; the selection returns to
            // the first one last, so a provider that rebinds to its same-family
            // twin shows in the settings and in the journal.
            let picks = [
                api_named(&models, "acme-proxy/gpt-4o"),
                api_named(&models, "openai/gpt-4o"),
                api_named(&models, "openrouter/gpt-4o"),
                api_named(&models, "openrouter/openrouter/free"),
                synthetic_named(&models, "acme/fast"),
                api_named(&models, "openai/gpt-4o"),
            ];
            for route in &picks {
                agent
                    .submit(Command::SetModel {
                        model: route.clone(),
                        save: Save::SessionOnly,
                    })
                    .await
                    .unwrap_or_else(|error| panic!("selecting {route:?}: {error}"));
                let view = agent.view(PageReq::default()).await.expect("view");
                assert_eq!(view.settings.model.as_ref(), Some(route));
            }
            let session = agent.session().to_string();
            // Selections alone stay lazy: one prompt creates the journal
            // (its model fails closed above).
            agent
                .submit(Command::Prompt {
                    expect: dal_core::Expect::Idle,
                    content: vec![dal_core::Part::Text {
                        text: "create the journal".into(),
                    }],
                })
                .await
                .unwrap_or_else(|error| panic!("the journal-creating prompt: {error}"));
            let wanted = [
                journaled("openai_chat", "gpt-4o", Some("acme-proxy")),
                journaled("openai_responses", "gpt-4o", Some("openai")),
                journaled("openai_responses", "gpt-4o", Some("openrouter")),
                journaled("openai_responses", "openrouter/free", Some("openrouter")),
                journaled("synthetic", "acme/fast", None),
                journaled("openai_responses", "gpt-4o", Some("openai")),
            ];
            let mut records = journal_models(&data, &session);
            for _ in 0..50 {
                if records.len() >= wanted.len() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                records = journal_models(&data, &session);
            }
            assert!(
                records.ends_with(&wanted),
                "journal model records {records:?} do not end with {wanted:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn subscribe_cursors_obey_predecessor_head_successor_boundaries() {
        let server = Loopback::start().await;
        let rig = catalog_rig(&server).await;
        let workspace = rig.core_workspace();
        with_remote(&rig, |host| async move {
            let models = host.models().await.expect("models decode");
            let agent = host
                .open(SessionRef::New {
                    workspace,
                    name: None,
                })
                .await
                .expect("open a session");
            let primed = api_named(&models, "openai/gpt-4o");
            agent
                .submit(Command::SetModel {
                    model: primed.clone(),
                    save: Save::SessionOnly,
                })
                .await
                .unwrap_or_else(|error| panic!("selecting {primed:?}: {error}"));
            let head = agent.view(PageReq::default()).await.expect("view");
            let before = dal_core::Seq::new(
                NonZeroU64::new(head.seq.get() - 1)
                    .expect("the priming selection leaves a predecessor"),
            );
            let mut predecessor = agent
                .subscribe(Some((head.r#gen, before)))
                .await
                .expect("subscribe at N-1");
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            loop {
                let got = tokio::time::timeout_at(deadline, predecessor.next())
                    .await
                    .expect("the retained update replays before the deadline")
                    .expect("no transport error on a suffix subscription");
                assert!(
                    matches!(got, RemoteDelivery::Update(_)),
                    "a retained predecessor must replay, never resync"
                );
                if let RemoteDelivery::Update(update) = got
                    && (update.r#gen, update.seq) == (head.r#gen, head.seq)
                {
                    break;
                }
            }
            let after = dal_core::Seq::new(
                NonZeroU64::new(head.seq.get() + 1).expect("successor seq stays nonzero"),
            );
            let after_text = after.get().to_string();
            let head_text = head.seq.get().to_string();
            match agent.subscribe(Some((head.r#gen, after))).await {
                Err(WireError::Protocol { code, message }) => {
                    assert_eq!(code, -32602, "a beyond-head cursor is invalid params");
                    for needle in ["session/subscribe", after_text.as_str(), head_text.as_str()] {
                        assert!(
                            message.contains(needle),
                            "the refusal names its context: {message}"
                        );
                    }
                }
                Err(other) => {
                    panic!("a beyond-head cursor must fail as invalid params, got {other:?}")
                }
                Ok(_) => panic!("a beyond-head cursor must be refused"),
            }
            let mut live = agent
                .subscribe(Some((head.r#gen, head.seq)))
                .await
                .expect("subscribe at the head");
            let second = api_named(&models, "openrouter/gpt-4o");
            agent
                .submit(Command::SetModel {
                    model: second.clone(),
                    save: Save::SessionOnly,
                })
                .await
                .unwrap_or_else(|error| panic!("selecting {second:?}: {error}"));
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            loop {
                let got = tokio::time::timeout_at(deadline, live.next())
                    .await
                    .expect("the selection arrives before the deadline")
                    .expect("no transport error on a live subscription");
                assert!(
                    matches!(got, RemoteDelivery::Update(_)),
                    "a live head subscription must not resync on a new selection"
                );
                if let RemoteDelivery::Update(update) = got
                    && matches!(
                        &update.kind,
                        dal_core::UpdateKind::Settings(s) if s.model.as_ref() == Some(&second)
                    )
                {
                    break;
                }
            }
            let view = agent.view(PageReq::default()).await.expect("view");
            assert_eq!(view.settings.model.as_ref(), Some(&second));
        })
        .await;
    }

    #[tokio::test]
    async fn unknown_provider_selection_survives_a_materializing_turn_and_journals_verbatim() {
        let server = Loopback::start().await;
        let rig = catalog_rig(&server).await;
        let workspace = rig.core_workspace();
        let data = rig.data();
        with_remote(&rig, |host| async move {
            let agent = host
                .open(SessionRef::New {
                    workspace,
                    name: None,
                })
                .await
                .expect("open a session");
            let ghost = ModelRoute::Api {
                family: Family::Responses,
                model: "gpt-4o".into(),
                provider: Some("ghost-provider".into()),
            };
            agent
                .submit(Command::SetModel {
                    model: ghost.clone(),
                    save: Save::SessionOnly,
                })
                .await
                .expect("verbatim submit never validates the provider");
            let head = agent.view(PageReq::default()).await.expect("view");
            assert_eq!(
                head.settings.model.as_ref(),
                Some(&ghost),
                "an unconfigured provider must never rebind to a twin"
            );
            let mut events = agent
                .subscribe(Some((head.r#gen, head.seq)))
                .await
                .expect("subscribe at the head");
            agent
                .submit(Command::Prompt {
                    expect: Expect::Idle,
                    content: vec![Part::Text {
                        text: "probe".into(),
                    }],
                })
                .await
                .expect("prompt starts a materializing turn");
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            loop {
                let got = tokio::time::timeout_at(deadline, events.next())
                    .await
                    .expect("the turn settles before the deadline")
                    .expect("no transport error while the turn runs");
                if let RemoteDelivery::Update(update) = got
                    && matches!(update.kind, dal_core::UpdateKind::TurnEnded { .. })
                {
                    break;
                }
            }
            let view = agent.view(PageReq::default()).await.expect("view");
            assert_eq!(
                view.settings.model.as_ref(),
                Some(&ghost),
                "the settled turn keeps the ghost selection"
            );
            let wanted = [journaled(
                "openai_responses",
                "gpt-4o",
                Some("ghost-provider"),
            )];
            let records = journal_models(&data, &agent.session().to_string());
            assert!(
                records.ends_with(&wanted),
                "journal model records {records:?} end with the ghost provider"
            );
        })
        .await;
    }
}
