//! Cloud Code Assist catalog discovery: the native family reads its model ids
//! from `retrieveUserQuota` with the sign-in's stored project, Antigravity
//! reads its roster from `fetchAvailableModels`, and neither invents a limit,
//! a capability, or a price the reply does not carry. Every test runs against
//! a scripted loopback server that records the exact request it answered.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    thread,
};

use super::*;
use crate::catalog::decode::{decode_antigravity_models, decode_cloud_code_quota};

const QUOTA_TOKEN: &str = "ya29-quota-token";
const PROJECT: &str = "stored-project-1";

/// One request the scripted server answered: the request line, the lowercased
/// request headers, and the whole body.
struct Captured {
    request_line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }

    fn body_text(&self) -> &str {
        std::str::from_utf8(&self.body).expect("a UTF-8 request body")
    }
}

/// Answers one request per `replies` entry, in order, each as
/// `(status, body)`, and returns every request it saw. A client that asks for
/// more than was scripted finds the listener closed.
fn serve_captured(
    listener: TcpListener,
    replies: Vec<(u16, Vec<u8>)>,
) -> thread::JoinHandle<Vec<Captured>> {
    thread::spawn(move || {
        let mut seen = Vec::with_capacity(replies.len());
        for (status, body) in replies {
            let mut stream = accept_loopback(&listener);
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set the replay socket read timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("clone the replay socket"));
            let mut request_line = String::new();
            reader
                .read_line(&mut request_line)
                .expect("read the request line");
            let mut headers = Vec::new();
            let mut length = 0_usize;
            loop {
                let mut line = String::new();
                let bytes = reader.read_line(&mut line).expect("read a request header");
                if bytes == 0 || line == "\r\n" {
                    break;
                }
                let (name, value) = line.split_once(':').expect("a header has a colon");
                let name = name.trim().to_ascii_lowercase();
                if name == "content-length" {
                    length = value.trim().parse().expect("a content length");
                }
                headers.push((name, value.trim().to_owned()));
            }
            let mut request_body = vec![0_u8; length];
            reader
                .read_exact(&mut request_body)
                .expect("read the request body");
            drop(reader);
            let head = format!(
                "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(head.as_bytes())
                .expect("write the replay head");
            stream.write_all(&body).expect("write the replay body");
            seen.push(Captured {
                request_line,
                headers,
                body: request_body,
            });
        }
        seen
    })
}

fn sign_in(project: Option<&str>) -> Credential {
    Credential::OAuth(OAuthCredential {
        access_token: SecretString::from(String::from(QUOTA_TOKEN)),
        refresh_token: SecretString::from(String::from("refresh-secret")),
        expires_at: None,
        id_token: None,
        account_id: None,
        project_id: project.map(String::from),
    })
}

/// One refresh of a provider of `family` against `replies`, with every
/// request the server saw.
async fn refresh(
    family: Family,
    id: &str,
    credential: &Credential,
    replies: Vec<(u16, Vec<u8>)>,
) -> (CatalogFetch, Vec<Captured>) {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the catalog listener");
    listener
        .set_nonblocking(true)
        .expect("make the catalog listener nonblocking");
    let port = listener
        .local_addr()
        .expect("read the listener port")
        .port();
    let server = serve_captured(listener, replies);
    let made = provider(id, family, &format!("http://127.0.0.1:{port}"));
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &made,
        credential,
        cache_dir: directory.path(),
        user_agent: "GeminiCLI/test-agent",
        version: "test",
        env: empty_env(),
    };
    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    (fetched, server.join().expect("serve the scripted replies"))
}

async fn native(
    credential: &Credential,
    replies: Vec<(u16, Vec<u8>)>,
) -> (CatalogFetch, Vec<Captured>) {
    refresh(
        Family::GoogleCloudCodeAssist,
        "google-cloud-code-assist",
        credential,
        replies,
    )
    .await
}

fn ids(fetched: &CatalogFetch) -> Vec<&str> {
    fetched
        .entries
        .iter()
        .map(|entry| entry.id.as_ref())
        .collect()
}

fn native_entry(base: &str) -> ProviderEntry {
    provider(
        "google-cloud-code-assist",
        Family::GoogleCloudCodeAssist,
        base,
    )
}

#[tokio::test]
async fn the_quota_reply_lists_only_gemini_ids_and_invents_no_limit_capability_or_price() {
    let body = br#"{"buckets":[
        {"modelId":"gemini-2.5-pro","remainingFraction":0.5},
        {"modelId":"gemini-3-flash-preview"},
        {"modelId":"gemini-2.5-pro","tokenType":"REQUESTS"},
        {"modelId":"chat_20706"},
        {"modelId":"  "},
        {"tokenType":"REQUESTS"},
        "not a bucket",
        {"modelId":7}
    ],"unknownMember":true}"#
        .to_vec();
    let credential = sign_in(Some(PROJECT));
    let (fetched, seen) = native(&credential, vec![(200, body)]).await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(
        ids(&fetched),
        ["gemini-2.5-pro", "gemini-3-flash-preview"],
        "wire ids, deduplicated, gemini class only, sorted"
    );
    for entry in &fetched.entries {
        assert_eq!(entry.provider.as_ref(), "google-cloud-code-assist");
        assert_eq!(entry.listing, Listing::Listed);
        assert_eq!(entry.context_window, None, "{}", entry.id);
        assert_eq!(entry.max_output, None, "{}", entry.id);
        assert!(entry.price.is_none(), "{}: no free price", entry.id);
        assert!(entry.premium_multiplier.is_none(), "{}", entry.id);
        assert!(!entry.image_input, "{}: no modality in the reply", entry.id);
        assert!(
            !entry.remote_compact,
            "{}: no compaction endpoint",
            entry.id
        );
        assert_eq!(entry.tool_support, ToolSupport::Any, "{}", entry.id);
    }
    let [request] = seen.as_slice() else {
        panic!("one quota request, saw {}", seen.len());
    };
    assert!(
        request
            .request_line
            .starts_with("POST /v1internal:retrieveUserQuota "),
        "{}",
        request.request_line
    );
    assert_eq!(request.body_text(), format!(r#"{{"project":"{PROJECT}"}}"#));
    let bearer = format!("Bearer {QUOTA_TOKEN}");
    assert_eq!(request.header("authorization"), Some(bearer.as_str()));
    assert_eq!(
        request.header("client-metadata"),
        Some("ideType=IDE_UNSPECIFIED,platform=PLATFORM_UNSPECIFIED,pluginType=GEMINI")
    );
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("user-agent"), Some("GeminiCLI/test-agent"));
    assert_eq!(request.header("accept"), Some("application/json"));
}

#[tokio::test]
async fn a_live_quota_list_is_the_catalog_and_the_cache_serves_it_after_a_refusal() {
    let directory = TestDir::new();
    let credential = sign_in(Some(PROJECT));
    let client = reqwest::Client::new();
    let mut sources = Vec::new();
    for replies in [
        vec![(
            200,
            br#"{"buckets":[{"modelId":"gemini-2.5-pro"}]}"#.to_vec(),
        )],
        vec![(403, br#"{"error":{"message":"denied"}}"#.to_vec())],
    ] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the catalog listener");
        listener
            .set_nonblocking(true)
            .expect("make the catalog listener nonblocking");
        let port = listener
            .local_addr()
            .expect("read the listener port")
            .port();
        let server = serve_captured(listener, replies);
        let made = native_entry(&format!("http://127.0.0.1:{port}"));
        let fetch = ModelFetch {
            client: &client,
            provider: &made,
            credential: &credential,
            cache_dir: directory.path(),
            user_agent: "dalgon/test",
            version: "test",
            env: empty_env(),
        };
        let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
        server.join().expect("serve the scripted reply");
        let listed = ids_owned(&fetched);
        sources.push((fetched.source, fetched.live_error, listed));
    }
    assert_eq!(sources[0].0, CatalogSource::Live);
    assert_eq!(sources[1].0, CatalogSource::Cache, "{:?}", sources[1].1);
    assert!(
        matches!(
            &sources[1].1,
            Some(ProviderError::Status { status: 403, .. })
        ),
        "{:?}",
        sources[1].1
    );
    assert_eq!(
        sources[1].2,
        ["gemini-2.5-pro"],
        "the cached wire id serves"
    );
}

fn ids_owned(fetched: &CatalogFetch) -> Vec<String> {
    fetched
        .entries
        .iter()
        .map(|entry| entry.id.to_string())
        .collect()
}

#[tokio::test]
async fn a_sign_in_without_a_project_or_an_api_key_sends_no_quota_request() {
    let blank = sign_in(Some("  "));
    let none = sign_in(None);
    for credential in [&blank, &none] {
        let (fetched, seen) = native(credential, Vec::new()).await;
        assert_eq!(fetched.source, CatalogSource::Typed);
        assert!(
            matches!(
                &fetched.live_error,
                Some(ProviderError::InvalidRequest { message })
                    if message.contains("no Google Cloud project")
                        && message.contains("dalgon login google-cloud-code-assist")
            ),
            "{:?}",
            fetched.live_error
        );
        assert!(seen.is_empty(), "no request carries an invented project");
    }
    let api_key = Credential::ApiKey {
        key: SecretString::from(String::from("key")),
    };
    let (fetched, seen) = native(&api_key, Vec::new()).await;
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::InvalidRequest { message }) if message.contains("OAuth")
        ),
        "{:?}",
        fetched.live_error
    );
    assert!(seen.is_empty());
    let (fetched, seen) = native(&Credential::None, Vec::new()).await;
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::NoCredentials { provider }) if provider == "google-cloud-code-assist"
        ),
        "{:?}",
        fetched.live_error
    );
    assert!(seen.is_empty());
}

#[tokio::test]
async fn a_refused_quota_request_degrades_to_typed_ids_without_echoing_a_secret() {
    let credential = sign_in(Some(PROJECT));
    let reply = format!(r#"{{"error":{{"message":"bad token {QUOTA_TOKEN}"}}}}"#).into_bytes();
    let (fetched, seen) = native(&credential, vec![(401, reply)]).await;
    assert_eq!(seen.len(), 1);
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(fetched.entries.is_empty());
    let error = fetched.live_error.expect("the refusal is reported");
    assert!(
        matches!(&error, ProviderError::Status { status: 401, .. }),
        "{error:?}"
    );
    let shown = error.to_string();
    assert!(!shown.contains(QUOTA_TOKEN), "{shown}");
    assert!(!shown.contains("refresh-secret"), "{shown}");
}

#[tokio::test]
async fn a_valid_quota_reply_that_lists_no_gemini_model_is_the_live_empty_list() {
    let credential = sign_in(Some(PROJECT));
    for body in [
        br#"{"buckets":[]}"#.as_slice(),
        br#"{"buckets":[{"modelId":"chat_20706"}]}"#,
        br"{}",
    ] {
        let (fetched, _) = native(&credential, vec![(200, body.to_vec())]).await;
        assert_eq!(fetched.source, CatalogSource::Live, "{body:?}");
        assert!(fetched.live_error.is_none(), "{body:?}");
        assert!(fetched.entries.is_empty(), "{body:?}: the wire lists none");
        // The wire fact stands: no id is invented for an account that lists none.
        let made = native_entry("http://127.0.0.1:1");
        let catalog = Catalog::with_sources(vec![(made, fetched.source)], fetched.entries);
        assert_eq!(
            resolve(&catalog, &[], "google-cloud-code-assist/gemini-2.5-pro").unwrap_err(),
            ResolveError::UnknownModel {
                reference: String::from("google-cloud-code-assist/gemini-2.5-pro"),
            }
        );
    }
}

#[test]
fn the_quota_decoder_refuses_a_reply_that_is_not_an_object_and_cleans_wire_ids() {
    let made = native_entry("http://127.0.0.1:1");
    for body in [&b"[]"[..], b"\"text\"", b"not json"] {
        assert!(
            matches!(
                decode_cloud_code_quota(&made, body),
                Err(ProviderError::Protocol { .. })
            ),
            "{body:?}"
        );
    }
    // A control or bidi character in a provider-supplied id is replaced, so a
    // compromised endpoint cannot smuggle terminal escapes into a listing.
    let rows = decode_cloud_code_quota(
        &made,
        "{\"buckets\":[{\"modelId\":\"Gemini-3.\\u202epro\"},{\"modelId\":\"gemini\"},\
         {\"modelId\":\"geminix\"},{\"modelId\":\"vendor/gemini-2.5-flash\"}]}"
            .as_bytes(),
    )
    .expect("the gemini-class ids decode");
    let listed: Vec<&str> = rows.iter().map(|row| row.id.as_ref()).collect();
    assert_eq!(
        listed,
        ["Gemini-3.\u{fffd}pro", "gemini", "vendor/gemini-2.5-flash"],
        "`geminix` is not the class; the bounded token also matches case-insensitively"
    );
}

/// The Antigravity roster, as the pinned discovery documents it.
fn roster() -> Vec<u8> {
    br#"{"models":{
        "gemini-3-flash-agent":{"displayName":"Gemini 3 Flash (Agent)","supportsImages":true,
            "supportsThinking":true,"maxTokens":1048576,"maxOutputTokens":65536,"recommended":true},
        "claude-sonnet-4-6":{"displayName":"  ","maxTokens":200000,"supportsImages":"yes"},
        "gemini-2.5-pro":{"displayName":"Denied","maxTokens":1},
        "chat_20706":{"displayName":"Internal chat"},
        "hidden-model":{"isInternal":true,"displayName":"Hidden"},
        "zero-limits":{"maxTokens":0,"maxOutputTokens":-5},
        "not-an-object":"text"
    },"agentModelSorts":[],"imageGenerationModelIds":[]}"#
        .to_vec()
}

#[tokio::test]
async fn the_antigravity_roster_carries_only_the_facts_the_reply_names() {
    let credential = sign_in(Some(PROJECT));
    let (fetched, seen) = refresh(
        Family::GoogleAntigravity,
        "google-antigravity",
        &credential,
        vec![(200, roster())],
    )
    .await;
    assert_eq!(
        fetched.source,
        CatalogSource::Live,
        "{:?}",
        fetched.live_error
    );
    assert_eq!(
        ids(&fetched),
        ["claude-sonnet-4-6", "gemini-3-flash-agent", "zero-limits"],
        "the denylist, the internal flag, and non-objects are dropped; rows sort by id"
    );
    let by_id = |id: &str| {
        fetched
            .entries
            .iter()
            .find(|entry| entry.id.as_ref() == id)
            .unwrap_or_else(|| panic!("{id} listed"))
    };
    let flash = by_id("gemini-3-flash-agent");
    assert_eq!(flash.display.as_ref(), "Gemini 3 Flash (Agent)");
    assert_eq!(flash.context_window, Some(1_048_576));
    assert_eq!(flash.max_output, Some(65_536));
    assert!(flash.image_input);
    let claude = by_id("claude-sonnet-4-6");
    assert_eq!(claude.display.as_ref(), "claude-sonnet-4-6", "blank name");
    assert_eq!(claude.context_window, Some(200_000));
    assert_eq!(claude.max_output, None, "no default output limit");
    assert!(
        !claude.image_input,
        "a non-boolean flag is not a capability"
    );
    let zero = by_id("zero-limits");
    assert_eq!((zero.context_window, zero.max_output), (None, None));
    for entry in &fetched.entries {
        assert!(
            entry.price.is_none(),
            "{}: the roster names no price",
            entry.id
        );
        assert_eq!(entry.tool_support, ToolSupport::Any, "{}", entry.id);
    }
    let [request] = seen.as_slice() else {
        panic!("one roster request, saw {}", seen.len());
    };
    assert!(
        request
            .request_line
            .starts_with("POST /v1internal:fetchAvailableModels "),
        "{}",
        request.request_line
    );
    assert_eq!(request.body_text(), "{}");
    let bearer = format!("Bearer {QUOTA_TOKEN}");
    assert_eq!(request.header("authorization"), Some(bearer.as_str()));
    assert_eq!(
        request.header("client-metadata"),
        None,
        "the Gemini CLI header never rides the Antigravity roster"
    );
}

#[tokio::test]
async fn the_antigravity_roster_degrades_like_every_other_list() {
    let credential = sign_in(Some(PROJECT));
    let (fetched, seen) = refresh(
        Family::GoogleAntigravity,
        "google-antigravity",
        &credential,
        vec![(500, b"{}".to_vec())],
    )
    .await;
    assert_eq!(seen.len(), 1);
    assert_eq!(fetched.source, CatalogSource::Typed);
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::Status { status: 500, .. })
        ),
        "{:?}",
        fetched.live_error
    );
    let (fetched, seen) = refresh(
        Family::GoogleAntigravity,
        "google-antigravity",
        &Credential::None,
        Vec::new(),
    )
    .await;
    assert!(seen.is_empty());
    assert!(
        matches!(
            &fetched.live_error,
            Some(ProviderError::NoCredentials { provider }) if provider == "google-antigravity"
        ),
        "{:?}",
        fetched.live_error
    );
    let made = provider("google-antigravity", Family::GoogleAntigravity, "http://x");
    for body in [
        &b"{}"[..],
        br#"{"models":[]}"#,
        br#"{"models":{"chat_20706":{}}}"#,
    ] {
        assert!(
            matches!(decode_antigravity_models(&made, body), Ok(rows) if rows.is_empty()),
            "{body:?}: a roster with no usable model is an empty list"
        );
    }
    for body in [&b"[]"[..], b"\"text\"", b"not json"] {
        assert!(
            matches!(
                decode_antigravity_models(&made, body),
                Err(ProviderError::Protocol { .. })
            ),
            "{body:?}"
        );
    }
}

#[test]
fn native_typed_rows_keep_tools_and_a_summary_fallback_while_the_local_cli_keeps_none() {
    let cloud = native_entry("http://127.0.0.1:1");
    let local = provider("my-gemini", Family::GoogleGeminiCli, "http://127.0.0.1:1");
    let catalog = Catalog::with_sources(
        vec![(cloud, CatalogSource::Typed), (local, CatalogSource::Typed)],
        Vec::new(),
    );
    let native = resolve(&catalog, &[], "google-cloud-code-assist/gemini-2.5-pro")
        .expect("the native typed id resolves");
    assert_eq!(native.entry.tool_support, ToolSupport::Any);
    assert!(
        !native.entry.remote_compact,
        "no remote compaction endpoint: history compacts locally"
    );
    assert_eq!(native.entry.context_window, None);
    assert_eq!(native.entry.max_output, None);
    assert!(native.entry.price.is_none());
    assert!(matches!(
        native.route,
        ModelRoute::Api {
            family: Family::GoogleCloudCodeAssist,
            ..
        }
    ));
    let subprocess =
        resolve(&catalog, &[], "my-gemini/gemini-2.5-pro").expect("the local typed id resolves");
    assert_eq!(subprocess.entry.tool_support, ToolSupport::None);
    assert!(matches!(
        subprocess.route,
        ModelRoute::Api {
            family: Family::GoogleGeminiCli,
            ..
        }
    ));
    // The two families never read as each other: a route naming the native
    // family cannot select the subprocess provider, and the reverse.
    for (family, provider_id) in [
        (Family::GoogleCloudCodeAssist, "my-gemini"),
        (Family::GoogleGeminiCli, "google-cloud-code-assist"),
    ] {
        let route = ModelRoute::Api {
            family,
            model: "gemini-2.5-pro".into(),
            provider: Some(provider_id.into()),
        };
        assert!(
            matches!(
                resolve_route(&catalog, &[], &route),
                Err(ResolveError::ProviderFamilyMismatch { .. })
            ),
            "{family:?} via {provider_id}"
        );
    }
}
