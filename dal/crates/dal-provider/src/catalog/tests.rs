use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use super::*;
use super::{
    cache::{read_cache_async, write_cache_async},
    decode::{decode_anthropic_page, decode_codex_models, decode_openai_models},
    prices::PRICE_ROWS,
    resolve::capability_row,
};
use crate::{
    auth::credential::{OAuthCredential, SecretString},
    provider::{AuthStyle, Transport},
    thinking::Effort,
};

fn provider(id: &str, family: Family, base_url: &str) -> ProviderEntry {
    ProviderEntry {
        id: id.into(),
        family,
        base_url: base_url.into(),
        transport: Transport::Https,
        key_env: None,
        auth: AuthStyle::Bearer,
        max_concurrent_requests: 1,
    }
}

fn row(provider: &str, id: &str, context_window: Option<u32>) -> CatalogEntry {
    CatalogEntry {
        provider: provider.into(),
        id: id.into(),
        display: id.into(),
        listing: Listing::Listed,
        context_window,
        max_output: None,
        thinking: ThinkingSupport::UnknownAdaptive,
        image_input: false,
        image_profile: None,
        remote_compact: false,
        supports_reasoning_summaries: false,
        tool_support: ToolSupport::Any,
        custom_grammar: false,
        temperature_allowed: false,
        display_supported: false,
    }
}

#[test]
fn aliases_expand_once_without_alias_chains() {
    let catalog = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://example.test/v1"),
            CatalogSource::Cache,
        )],
        vec![row("openai", "model", Some(42))],
    );
    let aliases = vec![
        (Box::<str>::from("first"), Box::<str>::from("second")),
        (Box::<str>::from("second"), Box::<str>::from("openai/model")),
    ];
    assert_eq!(
        resolve(&catalog, &aliases, "first")
            .unwrap_err()
            .to_string(),
        "unknown model second"
    );
}

#[test]
fn qualified_cache_miss_is_unknown_but_typed_source_accepts_new_ids() {
    let cached = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Cache,
        )],
        vec![row("openai", "known-model", Some(10_000))],
    );
    assert_eq!(
        resolve(&cached, &[], "openai/some-new-id")
            .unwrap_err()
            .to_string(),
        "unknown model openai/some-new-id"
    );
    assert_eq!(
        resolve(&cached, &[], "opneai/gpt-6-luna")
            .unwrap_err()
            .to_string(),
        "unknown model opneai/gpt-6-luna"
    );

    let typed = Catalog::with_sources(
        vec![(
            provider("openai", Family::Responses, "https://api.openai.com/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    let resolved = resolve(&typed, &[], "openai/some-new-id")
        .expect("provider without a list accepts typed ids");
    assert_eq!(resolved.entry.context_window, None);
}

#[test]
fn bare_id_collision_names_candidates_in_catalog_order() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("one", Family::Responses, "https://one.test/v1"),
                CatalogSource::Cache,
            ),
            (
                provider("two", Family::Anthropic, "https://two.test"),
                CatalogSource::Cache,
            ),
        ],
        vec![row("one", "shared", None), row("two", "shared", None)],
    );
    assert_eq!(
        resolve(&catalog, &[], "shared").unwrap_err().to_string(),
        "model id shared matches several providers: one/shared, two/shared"
    );
}

#[test]
fn first_slash_splits_provider_and_preserves_vendor_path() {
    let catalog = Catalog::with_sources(
        vec![(
            provider("zenmux", Family::Chat, "https://zenmux.test/v1"),
            CatalogSource::Cache,
        )],
        vec![row("zenmux", "openai/gpt-5.6-luna", None)],
    );
    let resolved = resolve(&catalog, &[], "zenmux/openai/gpt-5.6-luna")
        .expect("qualified z-mux model resolves");
    assert_eq!(resolved.route.id(), "openai/gpt-5.6-luna");
    assert_eq!(resolved.entry.context_window, None);
    let aliases = [(
        Box::<str>::from("fast"),
        Box::<str>::from("zenmux/openai/gpt-5.6-luna"),
    )];
    assert_eq!(
        resolve(&catalog, &aliases, "fast")
            .expect("configured alias resolves once")
            .route
            .id(),
        "openai/gpt-5.6-luna"
    );
}

#[test]
fn typed_vendor_segment_uses_last_capability_candidate() {
    let catalog = Catalog::with_sources(
        vec![(
            provider("zenmux", Family::Chat, "https://zenmux.test/v1"),
            CatalogSource::Typed,
        )],
        Vec::new(),
    );
    let resolved = resolve(&catalog, &[], "zenmux/openai/gpt-5.6-luna")
        .expect("typed vendor-prefixed id uses its built-in capability row");
    assert_eq!(resolved.entry.context_window, Some(922_000));
}

#[test]
fn date_suffix_uses_the_base_capability_row() {
    let catalog = Catalog::with_sources(
        vec![(
            provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
            CatalogSource::Cache,
        )],
        vec![row("anthropic", "claude-sonnet-5", Some(200_000))],
    );
    let resolved = resolve(&catalog, &[], "claude-sonnet-5-20260101")
        .expect("dated bare id matches its base row");
    assert_eq!(resolved.route.id(), "claude-sonnet-5-20260101");
    assert_eq!(resolved.entry.id.as_ref(), "claude-sonnet-5");
    assert_eq!(resolved.entry.context_window, Some(200_000));
}

#[test]
fn capability_lookup_prefers_exact_row_without_context_window() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let rows = [
        row("anthropic", "claude-sonnet-5", Some(200_000)),
        row("anthropic", "claude-sonnet-5-20260101", None),
    ];
    let matched = capability_row(&rows, &provider, "claude-sonnet-5-20260101")
        .expect("exact row with unknown context wins over the base row");
    assert_eq!(matched.id.as_ref(), "claude-sonnet-5-20260101");
    assert_eq!(matched.context_window, None);
}

#[test]
fn typed_resolution_uses_exact_compiled_temperature_pair() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Typed,
            ),
            (
                provider("zenmux", Family::Anthropic, "https://zenmux.test"),
                CatalogSource::Typed,
            ),
        ],
        Vec::new(),
    );
    let supported = resolve(&catalog, &[], "anthropic/claude-haiku-4-5")
        .expect("known compiled model resolves as typed");
    let wrong_provider = resolve(&catalog, &[], "zenmux/claude-haiku-4-5")
        .expect("known provider accepts typed ids");
    let unknown = resolve(&catalog, &[], "anthropic/claude-haiku-5")
        .expect("unknown model ids remain routable");
    assert!(supported.entry.temperature_allowed);
    assert!(!wrong_provider.entry.temperature_allowed);
    assert!(!unknown.entry.temperature_allowed);
}

#[test]
fn reserve_is_a_hidden_codex_row_with_luna_limits() {
    let catalog = Catalog::with_sources(
        vec![
            (
                provider(
                    "openai-codex",
                    Family::Codex,
                    "https://chatgpt.com/backend-api/codex",
                ),
                CatalogSource::Typed,
            ),
            (
                provider("openai", Family::Responses, "https://api.openai.com/v1"),
                CatalogSource::Typed,
            ),
            (
                provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
                CatalogSource::Typed,
            ),
        ],
        Vec::new(),
    );
    let reserve = resolve(&catalog, &[], "gpt-reserve").expect("hidden reserve row resolves");
    assert_eq!(reserve.provider.as_ref(), "openai-codex");
    assert_eq!(reserve.entry.listing, Listing::Hidden);
    assert_eq!(reserve.entry.display.as_ref(), "Luna Reserve");
    assert_eq!(reserve.entry.context_window, Some(258_400));
    assert!(reserve.entry.supports_reasoning_summaries);
    assert_eq!(
        resolve(&catalog, &[], "openai/gpt-reserve")
            .unwrap_err()
            .to_string(),
        "unknown model openai/gpt-reserve"
    );
    assert_eq!(
        resolve(&catalog, &[], "anthropic/gpt-reserve")
            .unwrap_err()
            .to_string(),
        "unknown model anthropic/gpt-reserve"
    );
}

#[test]
fn built_in_tool_support_is_api_and_effort_specific() {
    let entries = built_in_entries();
    let find = |id: &str| {
        entries
            .iter()
            .find(|entry| entry.id.as_ref() == id)
            .expect("built-in row exists")
    };
    let astra = find("gpt-6-astra").tool_support;
    assert_eq!(astra, ToolSupport::ResponsesOnly);
    assert!(!astra.allows(Family::Chat, ThinkingLevel::Off));
    assert!(astra.allows(Family::Responses, ThinkingLevel::High));

    let sol = find("gpt-6-sol").tool_support;
    assert_eq!(sol, ToolSupport::ChatWhenNoReasoning);
    assert!(sol.allows(Family::Chat, ThinkingLevel::Off));
    assert!(!sol.allows(Family::Chat, ThinkingLevel::High));
    assert!(sol.allows(Family::Responses, ThinkingLevel::High));

    let gpt_56 = find("gpt-5.6-luna").tool_support;
    assert_eq!(gpt_56, ToolSupport::Any);
    assert!(gpt_56.allows(Family::Chat, ThinkingLevel::High));
}

#[test]
fn openai_decoder_uses_exact_compiled_temperature_capability() {
    let provider = provider("openai", Family::Chat, "https://api.openai.com/v1");
    let rows = decode_openai_models(
        &provider,
        br#"{"data":[{"id":"gpt-4o"},{"id":"gpt-6-sol"}]}"#,
    )
    .expect("OpenAI model rows decode");
    assert!(rows[0].temperature_allowed);
    assert!(!rows[1].temperature_allowed);
}

#[test]
fn openai_catalog_row_exposes_its_complete_image_profile() {
    let provider = provider("openai", Family::Chat, "https://api.openai.com/v1");
    let rows = decode_openai_models(&provider, br#"{"data":[{"id":"gpt-6-sol"}]}"#)
        .expect("OpenAI model row decodes");
    assert_eq!(rows[0].image_profile, Some(ImageProfile::openai()));
}

#[test]
fn codex_decoder_hides_non_listed_rows_and_applies_window_percent() {
    let provider = provider(
        "openai-codex",
        Family::Codex,
        "https://chatgpt.com/backend-api/codex",
    );
    let bytes = br#"{"models":[{"slug":"gpt-6-luna","display_name":"GPT-6 Luna","visibility":"list","context_window":1000,"effective_context_window_percent":95,"supports_reasoning_summaries":true,"supported_reasoning_levels":["low",{"effort":"high"}],"input_modalities":["text","image"]},{"slug":"gpt-reserve","display_name":"Luna Reserve","visibility":"hide","context_window":1000}]}"#;
    let rows = decode_codex_models(&provider, bytes).expect("Codex rows decode");
    assert_eq!(rows[0].context_window, Some(950));
    assert_eq!(rows[0].listing, Listing::Listed);
    assert!(rows[0].image_input);
    assert!(rows[0].supports_reasoning_summaries);
    assert_eq!(rows[1].listing, Listing::Hidden);
    assert_eq!(rows[1].context_window, Some(950));
    assert!(!rows[1].supports_reasoning_summaries);
}

#[test]
fn anthropic_decoder_reads_limits_and_capabilities() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"claude-sonnet-5","display_name":"Claude Sonnet 5","max_input_tokens":200000,"max_tokens":8192,"capabilities":{"image_input":{"supported":true},"thinking":{"types":{"adaptive":{"supported":true},"disabled":{"supported":true}}},"effort":{"supported":true,"high":{"supported":true},"low":{"supported":true},"max":{"supported":true},"medium":{"supported":true},"xhigh":{"supported":true}},"context_management":{"compact_20260112":{"supported":true}}}}],"has_more":true,"last_id":"claude-sonnet-5"}"#;
    let page = decode_anthropic_page(&provider, bytes).expect("Anthropic page decodes");
    assert!(page.has_more);
    assert_eq!(page.last_id.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(page.rows[0].context_window, Some(200_000));
    assert_eq!(page.rows[0].max_output, Some(8192));
    assert!(page.rows[0].image_input);
    assert!(page.rows[0].remote_compact);
    if let ThinkingSupport::Adaptive {
        can_disable,
        accepted,
    } = &page.rows[0].thinking
    {
        assert!(*can_disable);
        assert_eq!(
            accepted,
            &[
                Effort::Low,
                Effort::Medium,
                Effort::High,
                Effort::Xhigh,
                Effort::Max,
            ]
        );
    } else {
        panic!("page did not report adaptive thinking");
    }
}

#[test]
fn anthropic_catalog_row_exposes_a_complete_profile_only_with_a_known_window() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"vision","max_input_tokens":200000,"capabilities":{"image_input":{"supported":true}}}]}"#;
    let page = decode_anthropic_page(&provider, bytes).expect("Anthropic page decodes");
    assert_eq!(
        page.rows[0].image_profile,
        Some(ImageProfile::anthropic_standard(200_000))
    );
}

#[test]
fn image_capability_without_known_billing_window_has_no_profile() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"vision","capabilities":{"image_input":{"supported":true}}}]}"#;
    let page = decode_anthropic_page(&provider, bytes).expect("Anthropic page decodes");
    assert!(page.rows[0].image_input);
    assert_eq!(page.rows[0].image_profile, None);
}

#[test]
fn anthropic_decoder_uses_exact_compiled_temperature_capability() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"claude-haiku-4-5"},{"id":"claude-sonnet-5"}],"has_more":false,"last_id":"claude-sonnet-5"}"#;
    let page = decode_anthropic_page(&provider, bytes)
        .expect("Anthropic model rows decode without extra capability fields");
    assert!(page.rows[0].temperature_allowed);
    assert!(!page.rows[1].temperature_allowed);
}

#[test]
fn anthropic_effort_rows_keep_only_reported_supported_levels() {
    let provider = provider("anthropic", Family::Anthropic, "https://api.anthropic.com");
    let bytes = br#"{"data":[{"id":"partial","capabilities":{"thinking":{"types":{"adaptive":{"supported":true}}},"effort":{"supported":true,"low":{"supported":true},"medium":{"supported":false},"high":{"supported":true},"xhigh":{"supported":true},"max":{"supported":false}}}},{"id":"disabled","capabilities":{"thinking":{"types":{"adaptive":{"supported":true}}},"effort":{"supported":false,"low":{"supported":true},"medium":{"supported":true}}}}],"has_more":false,"last_id":"disabled"}"#;
    let page = decode_anthropic_page(&provider, bytes).expect("Anthropic effort rows decode");
    let ThinkingSupport::Adaptive { accepted, .. } = &page.rows[0].thinking else {
        panic!("first row should have known adaptive thinking");
    };
    assert_eq!(
        accepted.as_slice(),
        &[Effort::Low, Effort::High, Effort::Xhigh]
    );
    let ThinkingSupport::Adaptive { accepted, .. } = &page.rows[1].thinking else {
        panic!("second row should remain known adaptive");
    };
    assert!(accepted.is_empty());
}

#[tokio::test]
async fn anthropic_fetch_paginates_and_atomically_caches_rows() {
    let directory = TestDir::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind catalog replay listener");
    listener
        .set_nonblocking(true)
        .expect("make catalog listener nonblocking");
    let port = listener.local_addr().expect("read listener port").port();
    let server = serve_anthropic_pages(
        listener,
        [
            r#"{"data":[{"id":"claude-one","max_input_tokens":200000,"max_tokens":8192}],"has_more":true,"last_id":"claude-one"}"#,
            r#"{"data":[{"id":"claude-two","max_input_tokens":300000,"max_tokens":8192}],"has_more":false,"last_id":"claude-two"}"#,
        ],
    );
    let mut provider = provider(
        "anthropic",
        Family::Anthropic,
        &format!("http://127.0.0.1:{port}"),
    );
    provider.auth = AuthStyle::XApiKey;
    let credential = Credential::ApiKey {
        key: SecretString::from(String::from("test-key")),
    };
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &credential,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };

    let fetched = load_models(&fetch, |_| std::future::pending::<()>()).await;
    let requests = server.join().expect("serve both model pages");
    assert_eq!(fetched.source, CatalogSource::Live);
    assert_eq!(fetched.entries.len(), 2);
    assert!(requests[0].contains("limit=1000"));
    assert!(requests[1].contains("after_id=claude-one"));
    let cached = read_cache_async(directory.path().join("models.json"))
        .await
        .expect("read atomically published cache");
    assert_eq!(
        cached
            .iter()
            .map(|entry| entry.id.as_ref())
            .collect::<Vec<_>>(),
        ["claude-one", "claude-two"]
    );
}
#[tokio::test]
async fn failed_live_fetch_uses_atomic_cache_then_typed_default_has_unknown_window() {
    let directory = TestDir::new();
    let port = closed_loopback_port();
    let provider = provider(
        "openai-codex",
        Family::Codex,
        &format!("http://127.0.0.1:{port}/backend-api/codex"),
    );
    let mut cached = row("openai-codex", "gpt-6-luna", Some(902_500));
    cached.supports_reasoning_summaries = true;
    let cache_path = directory.path().join("models.json");
    let oauth = Credential::OAuth(OAuthCredential {
        access_token: SecretString::from(String::from("token")),
        refresh_token: SecretString::from(String::from("refresh")),
        expires_at: None,
        id_token: None,
        account_id: Some(String::from("account")),
    });
    let client = reqwest::Client::new();
    let fetch = ModelFetch {
        client: &client,
        provider: &provider,
        credential: &oauth,
        cache_dir: directory.path(),
        user_agent: "dalgon/test",
        version: "test",
    };
    let other_provider_row = row("anthropic", "claude-sonnet-5", Some(200_000));
    write_cache_async(
        cache_path.clone(),
        "anthropic".into(),
        vec![other_provider_row],
    )
    .await
    .expect("write another provider's cache row");
    let other_provider_cache = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(other_provider_cache.source, CatalogSource::Typed);
    assert!(other_provider_cache.entries.is_empty());
    assert!(matches!(
        other_provider_cache.live_error.as_ref(),
        Some(ProviderError::Transport { .. })
    ));

    write_cache_async(cache_path.clone(), "openai-codex".into(), vec![cached])
        .await
        .expect("write Codex cache row");
    let all_cached = read_cache_async(cache_path.clone())
        .await
        .expect("read merged provider cache");
    let cached_providers: Vec<_> = all_cached
        .iter()
        .map(|entry| entry.provider.as_ref())
        .collect();
    assert_eq!(cached_providers.len(), 2);
    assert!(cached_providers.contains(&"anthropic"));
    assert!(cached_providers.contains(&"openai-codex"));
    let cached_result = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(cached_result.source, CatalogSource::Cache);
    assert_eq!(cached_result.entries[0].provider.as_ref(), "openai-codex");
    assert_eq!(cached_result.entries[0].context_window, Some(902_500));
    assert!(cached_result.entries[0].supports_reasoning_summaries);
    assert!(!cached_result.entries[0].temperature_allowed);
    assert!(!cached_result.entries[0].display_supported);

    fs::remove_file(cache_path).expect("remove cache");
    let typed_result = load_models(&fetch, |_| std::future::pending::<()>()).await;
    assert_eq!(typed_result.source, CatalogSource::Typed);
    assert!(typed_result.entries.is_empty());
    let catalog =
        Catalog::with_sources(vec![(provider, typed_result.source)], typed_result.entries);
    let typed = resolve(&catalog, &[], "openai-codex/some-new-id")
        .expect("known provider accepts a typed model id");
    assert_eq!(typed.route.id(), "some-new-id");
    assert_eq!(typed.entry.context_window, None);
    assert!(!typed.entry.temperature_allowed);
    assert!(!typed.entry.display_supported);
    assert!(matches!(
        typed.entry.thinking,
        ThinkingSupport::OpenAi {
            none_supported: true,
            ..
        }
    ));
}

#[tokio::test]
async fn version_one_cache_without_optional_capabilities_defaults_false() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    fs::write(
            &cache_path,
            br#"{"version":1,"entries":[{"provider":"anthropic","id":"unknown","display":"unknown","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"unknown_adaptive"},"image_input":false,"remote_compact":false},{"provider":"anthropic","id":"claude-haiku-4-5","display":"Claude Haiku 4.5","hidden":false,"context_window":null,"max_output":null,"thinking":{"kind":"unknown_adaptive"},"image_input":false,"remote_compact":false}]}"#,
        )
        .expect("write version-one cache");
    let entries = read_cache_async(cache_path)
        .await
        .expect("read version-one cache");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].image_profile, None);
    assert!(!entries[0].temperature_allowed);
    assert!(entries[1].temperature_allowed);
    assert!(!entries[0].display_supported && !entries[1].display_supported);
}

#[tokio::test]
async fn cached_capability_flags_survive_resolution() {
    let directory = TestDir::new();
    let cache_path = directory.path().join("models.json");
    let mut entry = row("anthropic", "synthetic-capability-model", Some(32_000));
    entry.image_input = true;
    entry.image_profile = Some(ImageProfile::anthropic_standard(32_000));
    entry.temperature_allowed = true;
    entry.display_supported = true;
    write_cache_async(cache_path.clone(), "anthropic".into(), vec![entry])
        .await
        .expect("write capability cache");
    let entries = read_cache_async(cache_path)
        .await
        .expect("read capability cache");
    let catalog = Catalog::with_sources(
        vec![(
            provider("anthropic", Family::Anthropic, "https://api.anthropic.com"),
            CatalogSource::Cache,
        )],
        entries,
    );
    let resolved = resolve(&catalog, &[], "anthropic/synthetic-capability-model")
        .expect("cached model resolves");
    assert_eq!(
        resolved.entry.image_profile,
        Some(ImageProfile::anthropic_standard(32_000))
    );
    assert!(resolved.entry.temperature_allowed && resolved.entry.display_supported);
}

fn serve_anthropic_pages(
    listener: TcpListener,
    pages: [&'static str; 2],
) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut request_lines = Vec::with_capacity(pages.len());
        for body in pages {
            let mut stream = accept_loopback(&listener);
            stream
                .set_nonblocking(false)
                .expect("make replay socket blocking");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set replay socket read timeout");
            let mut reader = BufReader::new(stream.try_clone().expect("clone replay socket"));
            let mut request_line = String::new();
            assert_ne!(
                reader
                    .read_line(&mut request_line)
                    .expect("read request line"),
                0
            );
            let mut header_line = String::new();
            loop {
                header_line.clear();
                let bytes = reader
                    .read_line(&mut header_line)
                    .expect("read request header");
                if bytes == 0 || header_line == "\r\n" {
                    break;
                }
            }
            drop(reader);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write replay response");
            request_lines.push(request_line);
        }
        request_lines
    })
}

fn accept_loopback(listener: &TcpListener) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return stream,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("catalog replay listener failed: {error}"),
        }
    }
}
struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "dal-provider-catalog-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("create catalog test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn closed_loopback_port() -> u16 {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback port");
    listener.local_addr().expect("read loopback port").port()
}

#[test]
fn exact_priced_model_and_missing_model_have_distinct_costs() {
    use dal_core::Usage;

    let row = PRICE_ROWS
        .iter()
        .find(|row| row.input.is_some() || row.output.is_some())
        .expect("snapshot has a priced row");
    let price = compiled_price(row.model).expect("snapshot model resolves");
    let usage = Usage {
        input_tokens: 1_000_000,
        cached_input_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: None,
        cache_write_tokens: 0,
        cost_usd: None,
    };
    assert_eq!(usage.cost_usd(None, Some(&price)), Some(price.input));
    assert!(compiled_price("missing/model").is_none());
    assert!(compiled_price(&format!("{}-unlisted", row.model)).is_none());
}

#[test]
fn generated_table_remains_sorted_for_binary_search() {
    assert!(
        PRICE_ROWS
            .windows(2)
            .all(|pair| pair[0].model < pair[1].model)
    );
    assert_eq!(price_source().0, "models.dev");
}
