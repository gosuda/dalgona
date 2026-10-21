//! Offline tests for the Bedrock auth chain: real temporary profiles and
//! stores, loopback HTTP for STS/SSO/ECS/IMDS, and real helper programs for
//! the process tier. No cloud call, no real credential, no ambient
//! environment mutation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_core::Family;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

use super::super::AwsCredential;
use super::metadata;
use super::profile;
use super::sso;
use super::{AwsCredentialResolver, AwsResolveOptions, BedrockAuth};
use crate::{
    AuthStore, AuthStyle, EnvSnapshot, ProviderEntry, ProviderError, ProviderSettings, Transport,
};

// ---------- scaffolding ----------

/// A unique scratch directory, removed on drop.
struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "dal-provider-aws-auth-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.path.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("scratch parents");
        }
        std::fs::write(&path, content).expect("scratch file");
        path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A hermetic snapshot: fixed `PATH`, the scratch home, the scratch cwd, and
/// IMDS disabled unless the test says otherwise.
fn snapshot(dir: &TestDir, extra: &[(&str, &str)]) -> EnvSnapshot {
    let mut vars: Vec<(&str, &str)> = vec![
        ("PATH", "test-path-value"),
        ("HOME", dir.path().to_str().expect("utf8 home")),
        ("AWS_EC2_METADATA_DISABLED", "true"),
    ];
    vars.extend_from_slice(extra);
    EnvSnapshot::test(&vars).with_cwd(dir.path().to_owned())
}

fn resolver(env: EnvSnapshot, home: &Path) -> AwsCredentialResolver {
    AwsCredentialResolver::new(env, crate::http::build_client(), Some(home.to_owned()))
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

fn cancel() -> CancellationToken {
    CancellationToken::new()
}

fn options(profile: Option<&str>, region: &str) -> AwsResolveOptions {
    AwsResolveOptions {
        profile: profile.map(str::to_owned),
        region: region.to_owned(),
    }
}

fn entry(id: &str) -> ProviderEntry {
    ProviderEntry {
        id: id.into(),
        def: None,
        family: Family::Bedrock,
        base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
        transport: Transport::Https,
        key_env: None,
        auth: AuthStyle::Bearer,
        max_concurrent_requests: 1,
        settings: ProviderSettings::default(),
    }
}

fn ctx_of<'a>(
    env: &'a EnvSnapshot,
    client: &'a reqwest::Client,
    home: &'a Path,
) -> super::ResolveCtx<'a> {
    super::ResolveCtx {
        env,
        client,
        home_dir: Some(home),
    }
}

// ---------- signed-versus-sent agreement ----------

/// Signs again, with the signer itself, the request a loopback server
/// received, taking every input from the received bytes: the `Host` header as
/// it arrived, the request target, the body, the date, and the values of the
/// named headers. The result equals the received `authorization` only when
/// what was signed is what was sent. It checks the agreement of the signer's
/// inputs with the wire; the signing algorithm has its own known-answer
/// tests.
fn matches_what_was_sent(
    hit: &Hit,
    credential: &AwsCredential,
    region: &str,
    service: &str,
    signed_headers: &[&str],
) -> bool {
    let Some(host) = hit.headers.get("host") else {
        return false;
    };
    let Some(long_date) = hit.headers.get("x-amz-date") else {
        return false;
    };
    let headers: Option<Vec<(&str, String)>> = signed_headers
        .iter()
        .map(|name| Some((*name, hit.headers.get(*name)?.clone())))
        .collect();
    let Some(headers) = headers else {
        return false;
    };
    let resigned = super::super::sigv4::SignedRequest {
        method: &hit.method,
        host,
        path: &hit.path,
        query: None,
        headers: &headers,
        service,
        region,
        credential,
        date: super::super::AmzDate {
            long: long_date.clone(),
            short: long_date.chars().take(8).collect(),
        },
    }
    .sign(&hit.body);
    hit.headers
        .get("authorization")
        .is_some_and(|received| *received == resigned.authorization)
}

// ---------- loopback HTTP ----------

/// One recorded request: method, path, lowercased headers, and raw body.
#[derive(Clone, Debug)]
struct Hit {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// One canned reply.
struct Canned {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Canned {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.as_bytes().to_vec(),
        }
    }

    fn xml(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "text/xml",
            body: body.as_bytes().to_vec(),
        }
    }

    fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: body.as_bytes().to_vec(),
        }
    }
}

/// A loopback server routing by request, with every hit recorded.
struct Server {
    base: String,
    hits: Arc<Mutex<Vec<Hit>>>,
    done: tokio::task::JoinHandle<()>,
}

async fn start_server(
    respond: impl Fn(&Hit) -> Canned + Send + Sync + 'static,
    budget: usize,
) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let hits: Arc<Mutex<Vec<Hit>>> = Arc::default();
    let serving = Arc::clone(&hits);
    #[expect(
        clippy::disallowed_methods,
        reason = "test edge: loopback HTTP server runs concurrently with the test"
    )]
    let done = tokio::spawn(async move {
        for _ in 0..budget {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let Some(hit) = read_request(&mut socket).await else {
                continue;
            };
            let canned = respond(&hit);
            serving.lock().expect("hits").push(hit);
            write_response(&mut socket, &canned).await;
        }
    });
    Server {
        base: format!("http://127.0.0.1:{port}"),
        hits,
        done,
    }
}

async fn stop_server(server: Server) -> Vec<Hit> {
    timeout(Duration::from_secs(10), server.done)
        .await
        .expect("server finishes")
        .expect("server joins");
    server.hits.lock().expect("hits").clone()
}

fn headers_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn read_request(socket: &mut TcpStream) -> Option<Hit> {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let count = socket.read(&mut chunk).await.ok()?;
        if count == 0 || raw.len() > 65_536 {
            return None;
        }
        raw.extend_from_slice(&chunk[..count]);
        if let Some(end) = headers_end(&raw) {
            let head = std::str::from_utf8(&raw[..end]).ok()?;
            let mut lines = head.split("\r\n");
            let mut words = lines.next()?.split(' ');
            let method = words.next()?.to_owned();
            let path = words.next()?.to_owned();
            let mut headers = HashMap::new();
            let mut content_length = 0_usize;
            for line in lines {
                let (name, value) = line.split_once(':')?;
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse().ok()?;
                }
                headers.insert(name.trim().to_lowercase(), value.trim().to_owned());
            }
            let body_start = end + 4;
            while raw.len() < body_start + content_length {
                let count = socket.read(&mut chunk).await.ok()?;
                if count == 0 {
                    return None;
                }
                raw.extend_from_slice(&chunk[..count]);
            }
            let body = raw[body_start..body_start + content_length].to_vec();
            return Some(Hit {
                method,
                path,
                headers,
                body,
            });
        }
    }
}

async fn write_response(socket: &mut TcpStream, canned: &Canned) {
    let reason = match canned.status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {} {reason}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        canned.status,
        canned.content_type,
        canned.body.len(),
    );
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(&canned.body).await;
}

// ---------- fixtures ----------

const STS_ROLE_XML: &str = concat!(
    "<AssumeRoleResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\">",
    "<AssumeRoleResult><Credentials>",
    "<AccessKeyId>TESTCHAINACCESSKEY</AccessKeyId>",
    "<SecretAccessKey>TESTCHAINSECRETKEYVALUE</SecretAccessKey>",
    "<SessionToken>TESTCHAINSESSIONTOKEN</SessionToken>",
    "<Expiration>2030-01-01T00:00:00Z</Expiration>",
    "</Credentials></AssumeRoleResult>",
    "<ResponseMetadata><RequestId>test-request</RequestId></ResponseMetadata>",
    "</AssumeRoleResponse>",
);

const STS_WEB_IDENTITY_XML: &str = concat!(
    "<AssumeRoleWithWebIdentityResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\">",
    "<AssumeRoleWithWebIdentityResult><Credentials>",
    "<AccessKeyId>TESTWEBIDENTITYKEY</AccessKeyId>",
    "<SecretAccessKey>TESTWEBIDENTITYSECRET</SecretAccessKey>",
    "<SessionToken>TESTWEBIDENTITYSESSION</SessionToken>",
    "<Expiration>2030-01-01T00:00:00Z</Expiration>",
    "</Credentials></AssumeRoleWithWebIdentityResult>",
    "<ResponseMetadata><RequestId>test-request</RequestId></ResponseMetadata>",
    "</AssumeRoleWithWebIdentityResponse>",
);

const STS_ERROR_XML: &str = concat!(
    "<ErrorResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\">",
    "<Error><Type>Sender</Type><Code>AccessDenied</Code>",
    "<Message>CANARY-errormessage-must-not-leak-0001</Message></Error>",
    "<RequestId>test-request</RequestId>",
    "</ErrorResponse>",
);

/// Routes STS POSTs by action: web identity first, because its action name
/// contains `AssumeRole` as a substring.
fn sts_routes(hit: &Hit) -> Canned {
    let body = String::from_utf8_lossy(&hit.body);
    if body.contains("Action=AssumeRoleWithWebIdentity") {
        return Canned::xml(200, STS_WEB_IDENTITY_XML);
    }
    if body.contains("Action=AssumeRole") {
        return Canned::xml(200, STS_ROLE_XML);
    }
    Canned::xml(400, STS_ERROR_XML)
}

const SSO_FEDERATION_JSON: &str = concat!(
    "{\"roleCredentials\":{\"accessKeyId\":\"TESTSSOACCESSKEY\",",
    "\"secretAccessKey\":\"TESTSSOSECRETKEY\",",
    "\"sessionToken\":\"TESTSSOSESSIONTOKEN\",\"expiration\":1893456000000}}",
);

const SSO_REFRESH_JSON: &str = concat!(
    "{\"accessToken\":\"CANARY-refreshed-token-0002\",",
    "\"expiresIn\":3600,\"refreshToken\":\"CANARY-rotated-refresh-0003\"}",
);

fn sso_token_file(start_url: &str, access_token: &str, expires_at: &str, extra: &str) -> String {
    format!(
        "{{\"accessToken\":\"{access_token}\",\"expiresAt\":\"{expires_at}\",\"startUrl\":\"{start_url}\",\"region\":\"us-east-1\"{extra}}}"
    )
}

// ---------- bearer seam ----------

#[tokio::test]
async fn bearer_env_short_circuits_the_chain() {
    let dir = TestDir::new("bearer-env");
    let env = snapshot(
        &dir,
        &[("AWS_BEARER_TOKEN_BEDROCK", "CANARY-bearer-token-0004")],
    );
    let resolver_instance = resolver(env, dir.path());
    let store = AuthStore::empty(dir.path().join("auth.json"));
    let auth = resolver_instance
        .resolve_bedrock_auth(
            &entry("amazon-bedrock"),
            &store,
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("bearer resolves");
    match auth {
        BedrockAuth::Bearer(token) => assert_eq!(token.expose(), "CANARY-bearer-token-0004"),
        BedrockAuth::SigV4(_) => panic!("bearer must bypass SigV4"),
        BedrockAuth::None => panic!("a bearer source must never resolve unsigned"),
    }
}

#[tokio::test]
async fn stored_key_beats_bearer_env() {
    let dir = TestDir::new("bearer-stored");
    let auth_path = dir.path().join("auth.json");
    crate::store_api_key(
        auth_path.clone(),
        "openai",
        String::from("CANARY-stored-key-0005"),
    )
    .await
    .expect("store");
    let store = AuthStore::load(&auth_path).expect("load");
    let env = snapshot(
        &dir,
        &[("AWS_BEARER_TOKEN_BEDROCK", "CANARY-bearer-token-0006")],
    );
    let resolver_instance = resolver(env, dir.path());
    let token = resolver_instance
        .resolve_bearer_token(&entry("openai"), &store)
        .expect("stored key");
    assert_eq!(token.expose(), "CANARY-stored-key-0005");
}

#[tokio::test]
async fn empty_bearer_env_counts_as_unset() {
    let dir = TestDir::new("bearer-empty");
    let env = snapshot(&dir, &[("AWS_BEARER_TOKEN_BEDROCK", "")]);
    let resolver_instance = resolver(env, dir.path());
    let store = AuthStore::empty(dir.path().join("auth.json"));
    assert!(
        resolver_instance
            .resolve_bearer_token(&entry("amazon-bedrock"), &store)
            .is_none()
    );
    let error = resolver_instance
        .resolve_bedrock_auth(
            &entry("amazon-bedrock"),
            &store,
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect_err("no source");
    assert!(matches!(error, ProviderError::NoCredentials { .. }));
}

#[tokio::test]
async fn auth_none_entry_resolves_no_credential_source() {
    let dir = TestDir::new("auth-none");
    let env = snapshot(
        &dir,
        &[("AWS_BEARER_TOKEN_BEDROCK", "CANARY-env-bearer-0007")],
    );
    let resolver_instance = resolver(env, dir.path());
    let auth_path = dir.path().join("auth.json");
    crate::store_api_key(auth_path, "openai", String::from("CANARY-stored-key-0008"))
        .await
        .expect("store");
    let store = AuthStore::load(dir.path().join("auth.json")).expect("load");
    let mut none_entry = entry("openai");
    none_entry.auth = AuthStyle::None;
    let auth = resolver_instance
        .resolve_bedrock_auth(
            &none_entry,
            &store,
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("auth none resolves unsigned without reading any source");
    assert!(
        matches!(auth, BedrockAuth::None),
        "auth=none must short-circuit before bearer and chain: {auth:?}"
    );
}

// ---------- chain precedence and profiles ----------

#[tokio::test]
async fn static_env_beats_profile_files() {
    let dir = TestDir::new("env-beats-file");
    dir.write(
        ".aws/credentials",
        "[default]\naws_access_key_id = TESTFILEKEY\naws_secret_access_key = TESTFILESECRET\n",
    );
    let env = snapshot(
        &dir,
        &[
            ("AWS_ACCESS_KEY_ID", "TESTENVKEY"),
            ("AWS_SECRET_ACCESS_KEY", "TESTENVSECRET"),
        ],
    );
    let resolver_instance = resolver(env, dir.path());
    let credential = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("env wins");
    assert_eq!(credential.access_key_id.as_ref(), "TESTENVKEY");
}

#[tokio::test]
async fn profile_static_keys_resolve_with_region() {
    let dir = TestDir::new("profile-static");
    dir.write(
        ".aws/credentials",
        "[testprofile]\naws_access_key_id = TESTPROFILEKEY\naws_secret_access_key = TESTPROFILESECRET\n",
    );
    dir.write(".aws/config", "[profile testprofile]\nregion = eu-west-1\n");
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    assert_eq!(
        resolver_instance
            .resolve_region(None, Some("testprofile"), &cancel(), deadline())
            .await,
        "eu-west-1"
    );
    let credential = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(Some("testprofile"), "eu-west-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("profile resolves");
    assert_eq!(credential.access_key_id.as_ref(), "TESTPROFILEKEY");
    assert!(credential.expires_at.is_none());
}

#[tokio::test]
async fn session_token_in_file_caps_the_cache() {
    let dir = TestDir::new("file-session");
    dir.write(
        ".aws/credentials",
        "[default]\naws_access_key_id = K\naws_secret_access_key = S\naws_session_token = T\n",
    );
    let before = super::now_unix();
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    let credential = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("session token");
    let expires = credential.expires_at.expect("capped");
    assert!(expires - before <= super::FILE_SESSION_CREDS_TTL_SECS + 5);
    assert!(expires - before >= super::FILE_SESSION_CREDS_TTL_SECS - 5);
}

// ---------- cache, invalidation, expiry, single-flight ----------

#[tokio::test]
async fn cache_serves_until_invalidate() {
    let dir = TestDir::new("cache-invalidate");
    let creds = dir.write(
        ".aws/credentials",
        "[default]\naws_access_key_id = TESTKEYONE\naws_secret_access_key = S\n",
    );
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    let first = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("first");
    assert_eq!(first.access_key_id.as_ref(), "TESTKEYONE");
    std::fs::write(
        &creds,
        "[default]\naws_access_key_id = TESTKEYTWO\naws_secret_access_key = S\n",
    )
    .expect("rewrite");
    let second = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("cached");
    assert_eq!(second.access_key_id.as_ref(), "TESTKEYONE");
    resolver_instance.invalidate(None, "us-east-1");
    let third = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("fresh");
    assert_eq!(third.access_key_id.as_ref(), "TESTKEYTWO");
}

#[cfg(unix)]
fn write_helper(dir: &TestDir, name: &str, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.write(name, script);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// A helper that outlives every test deadline. `snapshot()` pins `PATH` to a
/// placeholder and the runner clears the environment to the snapshot pairs,
/// so a bare `sleep` is "not found": the script would exit 127 at once and
/// never reach the cancel or deadline under test. Fixtures name `sleep` by
/// absolute path.
#[cfg(unix)]
const SLEEPING_HELPER: &str = "#!/bin/sh\n/bin/sleep 30\n";

#[cfg(unix)]
#[tokio::test]
async fn expired_process_output_never_caches() {
    let dir = TestDir::new("process-expiry");
    let count = dir.path().join("count");
    let helper = write_helper(
        &dir,
        "helper",
        "#!/bin/sh\necho x >> \"$1\"\nprintf '%s' '{\"Version\":1,\"AccessKeyId\":\"K\",\"SecretAccessKey\":\"S\",\"Expiration\":\"2000-01-01T00:00:00Z\"}'\n",
    );
    dir.write(
        ".aws/credentials",
        &format!(
            "[default]\ncredential_process = {} {}\n",
            helper.display(),
            count.display()
        ),
    );
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    for _ in 0..2 {
        let credential = resolver_instance
            .resolve_credentials(
                "amazon-bedrock",
                &options(None, "us-east-1"),
                &cancel(),
                deadline(),
            )
            .await
            .expect("past expiry still resolves once");
        assert!(
            credential
                .expires_at
                .is_some_and(|expires| expires < super::now_unix())
        );
    }
    let invocations = std::fs::read_to_string(&count)
        .expect("count")
        .lines()
        .count();
    assert_eq!(
        invocations, 2,
        "an already-expired credential never serves from cache"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn concurrent_cold_resolutions_share_one_fetch() {
    let dir = TestDir::new("single-flight");
    let count = dir.path().join("count");
    let helper = write_helper(
        &dir,
        "helper",
        "#!/bin/sh\n/bin/sleep 1\necho x >> \"$1\"\nprintf '%s' '{\"Version\":1,\"AccessKeyId\":\"TESTSHAREDKEY\",\"SecretAccessKey\":\"S\",\"Expiration\":\"2030-01-01T00:00:00Z\"}'\n",
    );
    dir.write(
        ".aws/credentials",
        &format!(
            "[default]\ncredential_process = {} {}\n",
            helper.display(),
            count.display()
        ),
    );
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    let shared_options = options(None, "us-east-1");
    let shared_cancel = cancel();
    let shared_deadline = deadline();
    let outcomes = futures::future::join_all((0..8).map(|_| {
        resolver_instance.resolve_credentials(
            "amazon-bedrock",
            &shared_options,
            &shared_cancel,
            shared_deadline,
        )
    }))
    .await;
    for outcome in &outcomes {
        assert_eq!(
            outcome.as_ref().expect("shared").access_key_id.as_ref(),
            "TESTSHAREDKEY"
        );
    }
    let invocations = std::fs::read_to_string(&count)
        .expect("count")
        .lines()
        .count();
    assert_eq!(invocations, 1, "one key performs one fetch");
}

#[cfg(unix)]
#[tokio::test]
async fn a_credential_process_without_expiration_signs_and_caches_for_the_session_window() {
    let dir = TestDir::new("process-no-expiration");
    let count = dir.path().join("count");
    let helper = write_helper(
        &dir,
        "helper",
        "#!/bin/sh\necho x >> \"$1\"\nprintf '%s' '{\"Version\":1,\"AccessKeyId\":\"K\",\"SecretAccessKey\":\"S\",\"SessionToken\":\"T\"}'\n",
    );
    dir.write(
        ".aws/credentials",
        &format!(
            "[default]\ncredential_process = {} {}\n",
            helper.display(),
            count.display()
        ),
    );
    let resolver_instance = resolver(snapshot(&dir, &[]), dir.path());
    let before = super::now_unix();
    let first = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("a token without Expiration resolves");
    // The helper named no lifetime: the signer must not read the capped
    // cache window as an expiry that has already passed.
    first
        .verify_usable(super::now_unix())
        .expect("a credential without Expiration signs");
    let expires = first.expires_at.expect("the cache window is capped");
    assert!(expires - before <= super::FILE_SESSION_CREDS_TTL_SECS + 5);
    assert!(expires - before >= super::FILE_SESSION_CREDS_TTL_SECS - 5);
    let second = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("served from the window");
    assert_eq!(second.access_key_id, first.access_key_id);
    let invocations = std::fs::read_to_string(&count)
        .expect("count")
        .lines()
        .count();
    assert_eq!(invocations, 1, "the window serves the second resolution");
}

#[cfg(unix)]
#[tokio::test]
async fn a_malformed_process_expiration_reads_like_a_missing_one() {
    let cases = [
        (
            r#"{"Version":1,"AccessKeyId":"K","SecretAccessKey":"S","SessionToken":"T","Expiration":"soon"}"#,
            true,
        ),
        (
            r#"{"Version":1,"AccessKeyId":"K","SecretAccessKey":"S","Expiration":"soon"}"#,
            false,
        ),
        (
            r#"{"Version":1,"AccessKeyId":"K","SecretAccessKey":"S"}"#,
            false,
        ),
    ];
    for (index, (envelope, has_token)) in cases.into_iter().enumerate() {
        let dir = TestDir::new(&format!("process-bad-expiration-{index}"));
        let helper = write_helper(
            &dir,
            "helper",
            &format!("#!/bin/sh\nprintf '%s' '{envelope}'\n"),
        );
        dir.write(
            ".aws/credentials",
            &format!("[default]\ncredential_process = {}\n", helper.display()),
        );
        let credential = resolver(snapshot(&dir, &[]), dir.path())
            .resolve_credentials(
                "amazon-bedrock",
                &options(None, "us-east-1"),
                &cancel(),
                deadline(),
            )
            .await
            .expect("the envelope resolves");
        assert!(
            credential.verify_usable(super::now_unix()).is_ok(),
            "{envelope}"
        );
        // A token caps the window; a bare key is long-lived.
        assert_eq!(credential.expires_at.is_some(), has_token, "{envelope}");
    }
}

// ---------- cancellation and deadline ----------

#[cfg(unix)]
#[tokio::test]
async fn a_waiter_behind_a_busy_resolution_obeys_its_own_cancellation_and_deadline() {
    let dir = TestDir::new("waiter");
    let helper = write_helper(&dir, "helper", SLEEPING_HELPER);
    dir.write(
        ".aws/credentials",
        &format!("[default]\ncredential_process = {}\n", helper.display()),
    );
    let resolver_instance = resolver(snapshot(&dir, &[]), dir.path());
    let shared_options = options(None, "us-east-1");
    let holder_cancel = cancel();
    let (holder, ()) = tokio::join!(
        resolver_instance.resolve_credentials(
            "amazon-bedrock",
            &shared_options,
            &holder_cancel,
            deadline(),
        ),
        async {
            // Let the holder reach its helper and take the key's slot.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let waiter_cancel = cancel();
            let stop_waiter = waiter_cancel.clone();
            let (cancelled, ()) = tokio::join!(
                resolver_instance.resolve_credentials(
                    "amazon-bedrock",
                    &shared_options,
                    &waiter_cancel,
                    deadline(),
                ),
                async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    stop_waiter.cancel();
                },
            );
            let error = cancelled.expect_err("the cancelled waiter refuses");
            assert!(format!("{error}").contains("cancelled"), "{error}");
            assert!(
                !holder_cancel.is_cancelled(),
                "the holder still runs when the waiter returns"
            );
            // A waiter whose own deadline passes refuses the same way.
            let error = resolver_instance
                .resolve_credentials(
                    "amazon-bedrock",
                    &shared_options,
                    &cancel(),
                    Instant::now() + Duration::from_millis(200),
                )
                .await
                .expect_err("the late waiter refuses");
            assert!(format!("{error}").contains("timed out"), "{error}");
            holder_cancel.cancel();
        },
    );
    let error = holder.expect_err("the holder is cancelled in the end");
    assert!(format!("{error}").contains("cancelled"), "{error}");
}

#[tokio::test]
async fn pre_cancelled_resolution_refuses() {
    let dir = TestDir::new("cancelled");
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    let token = cancel();
    token.cancel();
    let error = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &token,
            deadline(),
        )
        .await
        .expect_err("cancelled");
    assert!(format!("{error}").contains("cancelled"));
}

#[cfg(unix)]
#[tokio::test]
async fn mid_flight_cancel_kills_the_helper() {
    let dir = TestDir::new("cancel-midflight");
    let helper = write_helper(&dir, "helper", SLEEPING_HELPER);
    dir.write(
        ".aws/credentials",
        &format!("[default]\ncredential_process = {}\n", helper.display()),
    );
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    let token = cancel();
    let canceller = token.clone();
    #[expect(
        clippy::disallowed_methods,
        reason = "test edge: cancellation timer runs concurrently with the test"
    )]
    let stopper = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        canceller.cancel();
    });
    let error = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &token,
            deadline(),
        )
        .await
        .expect_err("cancelled");
    stopper.await.expect("stopper");
    let text = format!("{error}");
    assert!(text.contains("cancelled"), "{text}; typed error: {error:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn short_deadline_times_out() {
    let dir = TestDir::new("deadline");
    let helper = write_helper(&dir, "helper", SLEEPING_HELPER);
    dir.write(
        ".aws/credentials",
        &format!("[default]\ncredential_process = {}\n", helper.display()),
    );
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    let error = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            Instant::now() + Duration::from_millis(200),
        )
        .await
        .expect_err("deadline");
    // The deadline lands in the helper runner, whose refusal names the
    // resolution deadline, or in a shared-file read guarded by the
    // resolver's own "timed out" refusal.
    let text = format!("{error}");
    assert!(
        matches!(
            &error,
            ProviderError::Transport {
                family: Family::Bedrock,
                ..
            }
        ) && (text.contains("reached its resolution deadline") || text.contains("timed out")),
        "{text}; typed error: {error:?}"
    );
}

// ---------- error redaction ----------

#[cfg(unix)]
#[tokio::test]
async fn helper_stderr_never_enters_errors() {
    let dir = TestDir::new("redact-process");
    let helper = write_helper(
        &dir,
        "helper",
        "#!/bin/sh\necho 'CANARY-helper-secret-must-not-leak-0007' >&2\nexit 1\n",
    );
    dir.write(
        ".aws/credentials",
        &format!("[default]\ncredential_process = {}\n", helper.display()),
    );
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    let error = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect_err("nonzero");
    let text = format!("{error}");
    assert!(
        !text.contains("CANARY-helper-secret-must-not-leak-0007"),
        "{text}"
    );
    assert!(text.contains("exited"), "{text}");
}

#[cfg(unix)]
#[tokio::test]
async fn helper_stdout_never_enters_errors() {
    let dir = TestDir::new("redact-stdout");
    let helper = write_helper(
        &dir,
        "helper",
        "#!/bin/sh\nprintf '%s' '{\"Version\":\"CANARY-partial-secret-0008\"'\n",
    );
    dir.write(
        ".aws/credentials",
        &format!("[default]\ncredential_process = {}\n", helper.display()),
    );
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    let error = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect_err("invalid json");
    assert!(!format!("{error}").contains("CANARY-partial-secret-0008"));
}

#[tokio::test]
async fn sts_error_code_travels_without_body_text() {
    let server = start_server(|_| Canned::xml(403, STS_ERROR_XML), 1).await;
    let url: url::Url = format!("{}/", server.base).parse().expect("url");
    let endpoints = profile::StsEndpoints::loopback(&url).expect("loopback");
    let client = crate::http::build_client();
    let dir = TestDir::new("redact-sts");
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let error = profile::resolve_web_identity_at(
        &ctx,
        &endpoints,
        "arn:aws:iam::123456789012:role/Test",
        "test-token",
        None,
        &cancel(),
        deadline(),
    )
    .await
    .expect_err("denied");
    stop_server(server).await;
    let text = format!("{error}");
    assert!(
        !text.contains("CANARY-errormessage-must-not-leak-0001"),
        "{text}"
    );
    assert!(text.contains("HTTP 403"), "{text}");
    assert!(text.contains("AccessDenied"), "{text}");
}

#[tokio::test]
async fn sts_error_code_exception_spelling_travels_without_body_text() {
    const STS_ERROR_EXCEPTION_XML: &str = concat!(
        "<ErrorResponse xmlns=\"https://sts.amazonaws.com/doc/2011-06-15/\">",
        "<Error><Type>Sender</Type><Code>AccessDeniedException</Code>",
        "<Message>CANARY-errormessage-must-not-leak-0002</Message></Error>",
        "<RequestId>test-request-exception</RequestId>",
        "</ErrorResponse>",
    );
    let server = start_server(|_| Canned::xml(403, STS_ERROR_EXCEPTION_XML), 1).await;
    let url: url::Url = format!("{}/", server.base).parse().expect("url");
    let endpoints = profile::StsEndpoints::loopback(&url).expect("loopback");
    let client = crate::http::build_client();
    let dir = TestDir::new("redact-sts-exception");
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let error = profile::resolve_web_identity_at(
        &ctx,
        &endpoints,
        "arn:aws:iam::123456789012:role/Test",
        "test-token",
        None,
        &cancel(),
        deadline(),
    )
    .await
    .expect_err("denied");
    stop_server(server).await;
    let text = format!("{error}");
    assert!(
        !text.contains("CANARY-errormessage-must-not-leak-0002"),
        "{text}"
    );
    assert!(text.contains("HTTP 403"), "{text}");
    assert!(text.contains("AccessDeniedException"), "{text}");
}

// ---------- web identity and role chaining ----------

#[tokio::test]
async fn web_identity_exchanges_over_loopback() {
    let server = start_server(sts_routes, 1).await;
    let url: url::Url = format!("{}/", server.base).parse().expect("url");
    let endpoints = profile::StsEndpoints::loopback(&url).expect("loopback");
    let client = crate::http::build_client();
    let dir = TestDir::new("web-identity");
    let token_file = dir.write("token", "test-web-identity-token\n");
    let token = std::fs::read_to_string(&token_file).expect("token");
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let credential = profile::resolve_web_identity_at(
        &ctx,
        &endpoints,
        "arn:aws:iam::123456789012:role/Test",
        token.trim(),
        None,
        &cancel(),
        deadline(),
    )
    .await
    .expect("exchange");
    let hits = stop_server(server).await;
    assert_eq!(credential.access_key_id.as_ref(), "TESTWEBIDENTITYKEY");
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert!(
        hit.body
            .windows(b"AssumeRoleWithWebIdentity".len())
            .any(|window| window == b"AssumeRoleWithWebIdentity")
    );
    assert!(
        !hit.headers.contains_key("authorization"),
        "web identity is unsigned"
    );
    assert_eq!(
        hit.headers.get("content-type").map(String::as_str),
        Some("application/x-www-form-urlencoded")
    );
}

#[tokio::test]
async fn role_chain_signs_assume_role_with_base() {
    let server = start_server(sts_routes, 1).await;
    let url: url::Url = format!("{}/", server.base).parse().expect("url");
    let endpoints = profile::StsEndpoints::loopback(&url).expect("loopback");
    let client = crate::http::build_client();
    let dir = TestDir::new("role-chain");
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let set = profile::parse_set(
        "[profile chained]\nrole_arn = arn:aws:iam::123456789012:role/Chained\nsource_profile = base\n",
        "[base]\naws_access_key_id = TESTBASEKEY\naws_secret_access_key = TESTBASESECRET\n",
    )
    .expect("set");
    let credential = profile::resolve_profile_credentials_at(
        &ctx,
        &set,
        &endpoints,
        "us-east-1",
        "chained",
        &cancel(),
        deadline(),
    )
    .await
    .expect("chain")
    .expect("credential");
    let hits = stop_server(server).await;
    assert_eq!(credential.access_key_id.as_ref(), "TESTCHAINACCESSKEY");
    assert!(credential.session_token.is_some());
    assert_eq!(hits.len(), 1);
    let authorization = hits[0].headers.get("authorization").expect("signed");
    assert!(authorization.contains("Credential=TESTBASEKEY/"));
    assert!(authorization.contains("/sts/aws4_request"));
    // The loopback endpoint has a non-default port, so the `Host` header
    // carries it: the signature must cover exactly that value.
    assert!(
        hits[0]
            .headers
            .get("host")
            .is_some_and(|host| host.starts_with("127.0.0.1:")),
        "the request travels with a port in its Host header"
    );
    let base = AwsCredential {
        access_key_id: "TESTBASEKEY".into(),
        secret_access_key: "TESTBASESECRET".into(),
        session_token: None,
        expires_at: None,
    };
    assert!(
        matches_what_was_sent(&hits[0], &base, "us-east-1", "sts", &["content-type"]),
        "the STS signature covers the Host, target and body the server received"
    );
}

#[tokio::test]
async fn a_signed_converse_request_covers_what_the_loopback_server_received() {
    let server = start_server(|_| Canned::json(200, "{}"), 1).await;
    let credential = AwsCredential {
        access_key_id: "AKIDEXAMPLE".into(),
        secret_access_key: "TESTSECRETACCESSKEYVALUE".into(),
        session_token: Some("TESTSESSIONTOKENVALUE".into()),
        expires_at: None,
    };
    let wire = super::super::build_signed(
        &super::super::request_path("anthropic.claude-test:0"),
        br#"{"messages":[]}"#.to_vec(),
        &server.base,
        Some("us-east-1"),
        &credential,
        1_768_478_400,
    )
    .expect("the request signs");
    let response = wire
        .into_request(&crate::http::build_client())
        .send()
        .await
        .expect("the request sends");
    assert!(response.status().is_success());
    let hits = stop_server(server).await;
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert!(
        hit.headers
            .get("host")
            .is_some_and(|host| host.starts_with("127.0.0.1:")),
        "the request travels with a port in its Host header"
    );
    assert!(hit.path.contains("%3A"), "{}", hit.path);
    let signed = ["accept", "content-type"];
    assert!(
        matches_what_was_sent(hit, &credential, "us-east-1", "bedrock", &signed),
        "the Converse signature covers the Host, target, body and headers the server received"
    );
    // The check is not vacuous: a Host without its port, or a changed body,
    // no longer matches.
    let mut hostless = hit.clone();
    hostless
        .headers
        .insert(String::from("host"), String::from("127.0.0.1"));
    assert!(!matches_what_was_sent(
        &hostless,
        &credential,
        "us-east-1",
        "bedrock",
        &signed
    ));
    let mut tampered = hit.clone();
    tampered.body.push(b' ');
    assert!(!matches_what_was_sent(
        &tampered,
        &credential,
        "us-east-1",
        "bedrock",
        &signed
    ));
}

#[tokio::test]
async fn role_chain_cycle_is_refused() {
    let client = crate::http::build_client();
    let dir = TestDir::new("cycle");
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let set = profile::parse_set(
        "[profile a]\nrole_arn = arn:aws:iam::1:role/A\nsource_profile = b\n[profile b]\nrole_arn = arn:aws:iam::1:role/B\nsource_profile = a\n",
        "",
    )
    .expect("set");
    let prod = profile::StsEndpoints::production("us-east-1").expect("production");
    let error = profile::resolve_profile_credentials_at(
        &ctx,
        &set,
        &prod,
        "us-east-1",
        "a",
        &cancel(),
        deadline(),
    )
    .await
    .expect_err("cycle");
    assert!(format!("{error}").contains("cycle"));
}

#[tokio::test]
async fn mfa_gated_role_is_refused() {
    let client = crate::http::build_client();
    let dir = TestDir::new("mfa");
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let set = profile::parse_set(
        "[profile mfa]\nrole_arn = arn:aws:iam::1:role/M\nsource_profile = base\nmfa_serial = arn:aws:iam::1:mfa/u\n",
        "[base]\naws_access_key_id = K\naws_secret_access_key = S\n",
    )
    .expect("set");
    let prod = profile::StsEndpoints::production("us-east-1").expect("production");
    let error = profile::resolve_profile_credentials_at(
        &ctx,
        &set,
        &prod,
        "us-east-1",
        "mfa",
        &cancel(),
        deadline(),
    )
    .await
    .expect_err("mfa");
    assert!(format!("{error}").contains("MFA"));
}

// ---------- credential_process environment ----------

#[cfg(unix)]
#[tokio::test]
async fn helper_environment_is_snapshot_only() {
    let dir = TestDir::new("process-env");
    let sidecar = dir.path().join("env");
    let helper = write_helper(
        &dir,
        "helper",
        "#!/bin/sh\n/usr/bin/env > \"$1\"\nprintf '%s' '{\"Version\":1,\"AccessKeyId\":\"K\",\"SecretAccessKey\":\"S\"}'\n",
    );
    dir.write(
        ".aws/credentials",
        &format!(
            "[test-profile]\ncredential_process = {} {}\n",
            helper.display(),
            sidecar.display()
        ),
    );
    let env = snapshot(
        &dir,
        &[
            ("AWS_PROFILE", "test-profile"),
            ("TEST_HELPER_INPUT", "canary-input-value"),
        ],
    );
    let resolver_instance = resolver(env, dir.path());
    let _ = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("helper");
    let dumped: HashMap<String, String> = std::fs::read_to_string(&sidecar)
        .expect("sidecar")
        .lines()
        .filter_map(|line| {
            line.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect();
    // The helper inherits the full captured snapshot: a non-PATH/HOME
    // variable must travel, or the narrowing regresses silently.
    let mut expected = HashMap::new();
    expected.insert(String::from("PATH"), String::from("test-path-value"));
    expected.insert(
        String::from("HOME"),
        dir.path().to_str().expect("utf8").to_owned(),
    );
    expected.insert(
        String::from("AWS_EC2_METADATA_DISABLED"),
        String::from("true"),
    );
    expected.insert(String::from("AWS_PROFILE"), String::from("test-profile"));
    expected.insert(
        String::from("TEST_HELPER_INPUT"),
        String::from("canary-input-value"),
    );
    for key in dumped.keys() {
        assert!(
            expected.contains_key(key) || key == "PWD",
            "only snapshot pairs travel: {dumped:?}"
        );
    }
    for (key, value) in &expected {
        assert_eq!(
            dumped.get(key).map(String::as_str),
            Some(value.as_str()),
            "{key}"
        );
    }
    if let Some(pwd) = dumped.get("PWD") {
        assert_eq!(pwd.as_str(), dir.path().to_str().expect("utf8"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn helper_runs_in_the_captured_cwd() {
    let dir = TestDir::new("process-cwd");
    let sidecar = dir.path().join("pwd");
    let helper = write_helper(
        &dir,
        "helper",
        "#!/bin/sh\npwd > \"$1\"\nprintf '%s' '{\"Version\":1,\"AccessKeyId\":\"K\",\"SecretAccessKey\":\"S\"}'\n",
    );
    dir.write(
        ".aws/credentials",
        &format!(
            "[default]\ncredential_process = {} {}\n",
            helper.display(),
            sidecar.display()
        ),
    );
    let env = snapshot(&dir, &[]);
    let resolver_instance = resolver(env, dir.path());
    let _ = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect("helper");
    assert_eq!(
        std::fs::read_to_string(&sidecar).expect("sidecar").trim(),
        dir.path().to_str().expect("utf8")
    );
}

// ---------- ECS and IMDS ----------

#[tokio::test]
async fn container_full_uri_resolves() {
    let server = start_server(
        |_| {
            Canned::json(
                200,
                "{\"AccessKeyId\":\"TESTCONTAINERKEY\",\"SecretAccessKey\":\"TESTCONTAINERSECRET\",\"Token\":\"TESTCONTAINERTOKEN\",\"Expiration\":\"2030-01-01T00:00:00Z\"}",
            )
        },
        1,
    )
    .await;
    let dir = TestDir::new("ecs");
    let token_file = dir.write("token", "CANARY-container-token-0009\n");
    let env = snapshot(
        &dir,
        &[
            (
                "AWS_CONTAINER_CREDENTIALS_FULL_URI",
                &format!("{}/v2/credentials/abc", server.base),
            ),
            (
                "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
                token_file.to_str().expect("utf8"),
            ),
        ],
    );
    let client = crate::http::build_client();
    let ctx = ctx_of(&env, &client, dir.path());
    let credential = metadata::resolve_container_credentials(&ctx, &cancel(), deadline())
        .await
        .expect("ecs")
        .expect("credential");
    let hits = stop_server(server).await;
    assert_eq!(credential.access_key_id.as_ref(), "TESTCONTAINERKEY");
    assert_eq!(
        credential.session_token.as_deref(),
        Some("TESTCONTAINERTOKEN")
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].headers.get("authorization").map(String::as_str),
        Some("CANARY-container-token-0009")
    );
}

#[tokio::test]
async fn a_relative_container_token_file_resolves_against_the_captured_directory() {
    let server = start_server(
        |_| {
            Canned::json(
                200,
                "{\"AccessKeyId\":\"TESTCONTAINERKEY\",\"SecretAccessKey\":\"TESTCONTAINERSECRET\",\"Token\":\"TESTCONTAINERTOKEN\",\"Expiration\":\"2030-01-01T00:00:00Z\"}",
            )
        },
        1,
    )
    .await;
    let dir = TestDir::new("ecs-relative-token");
    dir.write("relative-token", "CANARY-relative-token-0024\n");
    // The scratch directory is the captured working directory; the test
    // process runs somewhere else, so only the captured value can find it.
    let env = snapshot(
        &dir,
        &[
            (
                "AWS_CONTAINER_CREDENTIALS_FULL_URI",
                &format!("{}/v2/credentials/abc", server.base),
            ),
            ("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE", "relative-token"),
        ],
    );
    let client = crate::http::build_client();
    let ctx = ctx_of(&env, &client, dir.path());
    let _ = metadata::resolve_container_credentials(&ctx, &cancel(), deadline())
        .await
        .expect("ecs")
        .expect("credential");
    let hits = stop_server(server).await;
    assert_eq!(
        hits[0].headers.get("authorization").map(String::as_str),
        Some("CANARY-relative-token-0024")
    );
}

#[tokio::test]
async fn container_full_uri_refuses_remote_http() {
    let dir = TestDir::new("ecs-refuse");
    let env = snapshot(
        &dir,
        &[(
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            "http://192.0.2.1/creds",
        )],
    );
    let client = crate::http::build_client();
    let ctx = ctx_of(&env, &client, dir.path());
    let error = metadata::resolve_container_credentials(&ctx, &cancel(), deadline())
        .await
        .expect_err("refused");
    assert!(matches!(error, ProviderError::InvalidRequest { .. }));
}

#[test]
fn endpoint_admission_cases() {
    assert!(metadata::ecs_relative_endpoint("/v2/credentials/abc").is_ok());
    assert!(metadata::ecs_relative_endpoint("//evil").is_err());
    assert!(metadata::ecs_relative_endpoint("relative").is_err());
    assert!(metadata::ecs_relative_endpoint("/back\\slash").is_err());
    assert!(metadata::ecs_full_endpoint("https://credentials.example.com/v1").is_ok());
    assert!(metadata::ecs_full_endpoint("http://127.0.0.1:8080/v1").is_ok());
    assert!(metadata::ecs_full_endpoint("http://192.0.2.1/v1").is_err());
    assert!(metadata::ecs_full_endpoint("ftp://example.com/v1").is_err());
    let endpoint = metadata::ecs_relative_endpoint("/v2/credentials/abc").expect("base");
    assert_eq!(endpoint.host_str(), Some("169.254.170.2"));
}

#[test]
fn container_full_uri_refuses_dns_names_that_only_start_like_a_local_address() {
    // Four dot-separated labels whose first two are digits are a DNS name,
    // not an IPv4 literal, when the last two are words. A remote attacker
    // controls such a name, so plain HTTP to it would carry the container
    // authorization token off the machine.
    for url in [
        "http://127.0.evil.example/creds",
        "http://10.0.evil.example/creds",
        "http://169.254.evil.example/creds",
        "http://192.168.evil.example/creds",
        "http://172.16.evil.example/creds",
        "http://0.0.evil.example/creds",
        // Single-label names that merely spell an IPv6 prefix.
        "http://fe80/creds",
        "http://fd12/creds",
    ] {
        let error = metadata::ecs_full_endpoint(url).expect_err(url);
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "{url}: {error:?}"
        );
    }
    for host in ["10.0.evil.example", "169.254.evil.example", "fe80", "fd12"] {
        assert!(!metadata::is_local_or_metadata_host(host), "{host}");
    }
}

#[test]
fn container_full_uri_admits_plain_http_by_resolved_host_not_by_text() {
    // Every URL below names a local address somewhere in its text, yet the
    // host the URL parser resolves is remote or outside the local ranges, so
    // the container authorization token must never be sent there.
    for url in [
        "http://127.0.0.1.evil.example/creds",
        "http://localhost.evil.example/creds",
        "http://127.0.0.1@evil.example/creds",
        "http://localhost:80@evil.example/creds",
        "http://evil.example/?127.0.0.1",
        "http://evil.example/#localhost",
        "http://evil.example\\@127.0.0.1/creds",
        "http://169.254.170.2.evil.example/creds",
        "http://evil.example:80/127.0.0.1",
        "http://[2001:db8::1]/creds",
        "http://[fb00::1]/creds",
        "http://[fec0::1]/creds",
    ] {
        let error = metadata::ecs_full_endpoint(url).expect_err(url);
        assert!(
            matches!(error, ProviderError::InvalidRequest { .. }),
            "{url}: {error:?}"
        );
    }
    // The admitted side of the same boundary, including both edges of the
    // IPv6 unique-local (fc00::/7) and link-local (fe80::/10) ranges.
    for (url, host) in [
        ("http://[fc00::1]/c", "[fc00::1]"),
        ("http://[fdff::1]/c", "[fdff::1]"),
        ("http://[fe80::1]/c", "[fe80::1]"),
        ("http://[febf::1]/c", "[febf::1]"),
        ("http://[::1]:9/c", "[::1]"),
        ("http://LOCALHOST/c", "localhost"),
        ("http://169.254.170.23/c", "169.254.170.23"),
    ] {
        let endpoint = metadata::ecs_full_endpoint(url).expect(url);
        assert_eq!(endpoint.host_str(), Some(host), "{url}");
    }
}

#[tokio::test]
async fn imds_chain_resolves_over_loopback() {
    let server = start_server(
        |hit| match (hit.method.as_str(), hit.path.as_str()) {
            ("PUT", "/latest/api/token") => {
                assert_eq!(
                    hit.headers.get("x-aws-ec2-metadata-token-ttl-seconds").map(String::as_str),
                    Some("21600")
                );
                Canned::text(200, "TESTIMDSTOKEN")
            }
            ("GET", "/latest/meta-data/iam/security-credentials/") => Canned::text(200, "test-role"),
            ("GET", "/latest/meta-data/iam/security-credentials/test-role") => {
                assert_eq!(
                    hit.headers.get("x-aws-ec2-metadata-token").map(String::as_str),
                    Some("TESTIMDSTOKEN")
                );
                Canned::json(
                    200,
                    "{\"AccessKeyId\":\"TESTIMDSKEY\",\"SecretAccessKey\":\"TESTIMDSSECRET\",\"Token\":\"TESTIMDSTOKEN2\",\"Expiration\":\"2030-01-01T00:00:00Z\"}",
                )
            }
            _ => Canned::text(404, "nope"),
        },
        3,
    )
    .await;
    let dir = TestDir::new("imds");
    let env = snapshot(
        &dir,
        &[
            ("AWS_EC2_METADATA_SERVICE_ENDPOINT", &server.base),
            ("AWS_EC2_METADATA_DISABLED", "false"),
        ],
    );
    let client = crate::http::build_client();
    let ctx = ctx_of(&env, &client, dir.path());
    let credential = metadata::resolve_imds_credentials(&ctx, &cancel(), deadline())
        .await
        .expect("imds")
        .expect("credential");
    let hits = stop_server(server).await;
    assert_eq!(credential.access_key_id.as_ref(), "TESTIMDSKEY");
    assert_eq!(hits.len(), 3);
}

#[tokio::test]
async fn non_regular_shared_file_reads_as_absent() {
    let dir = TestDir::new("dir-as-file");
    std::fs::create_dir_all(dir.path().join(".aws")).expect("aws dir");
    let env = snapshot(
        &dir,
        &[(
            "AWS_SHARED_CREDENTIALS_FILE",
            dir.path().join(".aws").to_str().expect("utf8"),
        )],
    );
    let resolver_instance = resolver(env, dir.path());
    let error = resolver_instance
        .resolve_credentials(
            "amazon-bedrock",
            &options(None, "us-east-1"),
            &cancel(),
            deadline(),
        )
        .await
        .expect_err("no source in a directory");
    assert!(matches!(error, ProviderError::NoCredentials { .. }));
}

#[tokio::test]
async fn imds_disabled_skips_the_probe() {
    let server = start_server(|_| Canned::text(200, "unreached"), 1).await;
    let base = server.base.clone();
    // The budget-1 server is never awaited: no request may arrive.
    let dir = TestDir::new("imds-disabled");
    let env = snapshot(
        &dir,
        &[
            ("AWS_EC2_METADATA_SERVICE_ENDPOINT", &base),
            ("AWS_EC2_METADATA_DISABLED", "true"),
        ],
    );
    let client = crate::http::build_client();
    let ctx = ctx_of(&env, &client, dir.path());
    let outcome = metadata::resolve_imds_credentials(&ctx, &cancel(), deadline())
        .await
        .expect("skipped");
    assert!(outcome.is_none());
    assert!(server.hits.lock().expect("hits").is_empty());
    server.done.abort();
}

#[tokio::test]
async fn imds_failure_is_soft() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let dir = TestDir::new("imds-soft");
    let env = snapshot(
        &dir,
        &[
            (
                "AWS_EC2_METADATA_SERVICE_ENDPOINT",
                &format!("http://127.0.0.1:{port}"),
            ),
            ("AWS_EC2_METADATA_DISABLED", "false"),
        ],
    );
    let client = crate::http::build_client();
    let ctx = ctx_of(&env, &client, dir.path());
    let outcome = metadata::resolve_imds_credentials(&ctx, &cancel(), deadline())
        .await
        .expect("soft");
    assert!(outcome.is_none());
}

// ---------- SSO ----------

#[tokio::test]
async fn sso_federation_uses_the_cached_token() {
    let server = start_server(
        |hit| {
            assert_eq!(
                hit.headers
                    .get("x-amz-sso_bearer_token")
                    .map(String::as_str),
                Some("CANARY-cached-sso-token-0010")
            );
            assert!(hit.path.starts_with("/federation/credentials"));
            assert!(hit.path.contains("account_id=123456789012"));
            assert!(hit.path.contains("role_name=TestRole"));
            Canned::json(200, SSO_FEDERATION_JSON)
        },
        1,
    )
    .await;
    let portal: url::Url = format!("{}/", server.base).parse().expect("url");
    let endpoints = sso::SsoEndpoints::loopback(&portal, &portal).expect("loopback");
    let client = crate::http::build_client();
    let dir = TestDir::new("sso-happy");
    dir.write(
        ".aws/sso/cache/token.json",
        &sso_token_file(
            "https://example.awsapps.com/start",
            "CANARY-cached-sso-token-0010",
            "2030-01-01T00:00:00Z",
            "",
        ),
    );
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let credential = sso::federation_flow(
        &ctx,
        "sso",
        "https://example.awsapps.com/start",
        "123456789012",
        "TestRole",
        &endpoints,
        &cancel(),
        deadline(),
    )
    .await
    .expect("federation");
    stop_server(server).await;
    assert_eq!(credential.access_key_id.as_ref(), "TESTSSOACCESSKEY");
    assert_eq!(credential.expires_at, Some(1_893_456_000));
}

#[tokio::test]
async fn sso_the_freshest_matching_cache_token_wins() {
    let server = start_server(
        |hit| {
            assert_eq!(
                hit.headers
                    .get("x-amz-sso_bearer_token")
                    .map(String::as_str),
                Some("CANARY-fresh-sso-token-0020")
            );
            Canned::json(200, SSO_FEDERATION_JSON)
        },
        1,
    )
    .await;
    let portal: url::Url = format!("{}/", server.base).parse().expect("url");
    let endpoints = sso::SsoEndpoints::loopback(&portal, &portal).expect("loopback");
    let client = crate::http::build_client();
    let dir = TestDir::new("sso-freshest");
    let start_url = "https://example.awsapps.com/start";
    // Sorted first, expired: a leftover of an earlier login.
    dir.write(
        ".aws/sso/cache/a.json",
        &sso_token_file(
            start_url,
            "CANARY-stale-sso-token-0021",
            "2000-01-01T00:00:00Z",
            "",
        ),
    );
    // Another portal's token that outlives every other file.
    dir.write(
        ".aws/sso/cache/b.json",
        &sso_token_file(
            "https://other.awsapps.com/start",
            "CANARY-foreign-sso-token-0022",
            "2090-01-01T00:00:00Z",
            "",
        ),
    );
    // The token the CLI keeps refreshed.
    dir.write(
        ".aws/sso/cache/c.json",
        &sso_token_file(
            start_url,
            "CANARY-fresh-sso-token-0020",
            "2031-01-01T00:00:00Z",
            "",
        ),
    );
    // A matching token with no parseable expiry ranks below any dated one.
    dir.write(
        ".aws/sso/cache/d.json",
        &sso_token_file(start_url, "CANARY-undated-sso-token-0023", "someday", ""),
    );
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let credential = sso::federation_flow(
        &ctx,
        "sso",
        start_url,
        "123456789012",
        "TestRole",
        &endpoints,
        &cancel(),
        deadline(),
    )
    .await
    .expect("the fresh token federates");
    stop_server(server).await;
    assert_eq!(credential.access_key_id.as_ref(), "TESTSSOACCESSKEY");
}

#[tokio::test]
async fn sso_refresh_persists_and_preserves() {
    let server = start_server(
        |hit| {
            if hit.path == "/token" {
                assert!(
                    hit.body
                        .windows(b"refresh_token".len())
                        .any(|window| window == b"refresh_token")
                );
                return Canned::json(200, SSO_REFRESH_JSON);
            }
            assert_eq!(
                hit.headers
                    .get("x-amz-sso_bearer_token")
                    .map(String::as_str),
                Some("CANARY-refreshed-token-0002")
            );
            Canned::json(200, SSO_FEDERATION_JSON)
        },
        2,
    )
    .await;
    let portal: url::Url = format!("{}/", server.base).parse().expect("url");
    let endpoints = sso::SsoEndpoints::loopback(&portal, &portal).expect("loopback");
    let client = crate::http::build_client();
    let dir = TestDir::new("sso-refresh");
    let cache = dir.write(
        ".aws/sso/cache/old.json",
        &sso_token_file(
            "https://example.awsapps.com/start",
            "CANARY-stale-token-0011",
            "2000-01-01T00:00:00Z",
            ",\"refreshToken\":\"CANARY-refresh-grant-0012\",\"clientId\":\"cid\",\"clientSecret\":\"CANARY-client-secret-0013\",\"customField\":\"keep-me\"",
        ),
    );
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let credential = sso::federation_flow(
        &ctx,
        "sso",
        "https://example.awsapps.com/start",
        "123456789012",
        "TestRole",
        &endpoints,
        &cancel(),
        deadline(),
    )
    .await
    .expect("refresh flow");
    stop_server(server).await;
    assert_eq!(credential.access_key_id.as_ref(), "TESTSSOACCESSKEY");
    let persisted = std::fs::read_to_string(&cache).expect("cache");
    assert!(
        persisted.contains("CANARY-refreshed-token-0002"),
        "{persisted}"
    );
    assert!(
        persisted.contains("keep-me"),
        "unknown members survive: {persisted}"
    );
    assert!(
        !persisted.contains("CANARY-stale-token-0011"),
        "{persisted}"
    );
}

#[tokio::test]
async fn sso_expired_without_grant_errors() {
    let client = crate::http::build_client();
    let dir = TestDir::new("sso-expired");
    dir.write(
        ".aws/sso/cache/old.json",
        &sso_token_file(
            "https://example.awsapps.com/start",
            "CANARY-stale-token-0014",
            "2000-01-01T00:00:00Z",
            "",
        ),
    );
    let portal: url::Url = "http://127.0.0.1:1/".parse().expect("url");
    let endpoints = sso::SsoEndpoints::loopback(&portal, &portal).expect("loopback");
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let error = sso::federation_flow(
        &ctx,
        "sso",
        "https://example.awsapps.com/start",
        "123456789012",
        "TestRole",
        &endpoints,
        &cancel(),
        deadline(),
    )
    .await
    .expect_err("expired");
    assert!(format!("{error}").contains("expired"));
}

#[tokio::test]
async fn sso_missing_cache_errors() {
    let client = crate::http::build_client();
    let dir = TestDir::new("sso-missing");
    let portal: url::Url = "http://127.0.0.1:1/".parse().expect("url");
    let endpoints = sso::SsoEndpoints::loopback(&portal, &portal).expect("loopback");
    let env = snapshot(&dir, &[]);
    let ctx = ctx_of(&env, &client, dir.path());
    let error = sso::federation_flow(
        &ctx,
        "sso",
        "https://example.awsapps.com/start",
        "123456789012",
        "TestRole",
        &endpoints,
        &cancel(),
        deadline(),
    )
    .await
    .expect_err("missing");
    let text = format!("{error}");
    assert!(text.contains("aws sso login"), "{text}");
}

// ---------- pure units ----------

#[tokio::test]
async fn region_precedence_is_explicit_env_profile_default() {
    let dir = TestDir::new("region");
    dir.write(".aws/config", "[profile p]\nregion = eu-west-1\n");
    let home = dir.path().to_str().expect("utf8").to_owned();
    let base = EnvSnapshot::test(&[("HOME", home.as_str())]).with_cwd(dir.path().to_owned());
    let resolver_instance = resolver(base, dir.path());
    assert_eq!(
        resolver_instance
            .resolve_region(Some("ap-south-1"), Some("p"), &cancel(), deadline())
            .await,
        "ap-south-1"
    );
    let with_env = EnvSnapshot::test(&[("HOME", home.as_str()), ("AWS_REGION", "us-west-2")])
        .with_cwd(dir.path().to_owned());
    assert_eq!(
        resolver(with_env, dir.path())
            .resolve_region(None, Some("p"), &cancel(), deadline())
            .await,
        "us-west-2"
    );
    assert_eq!(
        resolver_instance
            .resolve_region(None, Some("p"), &cancel(), deadline())
            .await,
        "eu-west-1"
    );
    assert_eq!(
        resolver_instance
            .resolve_region(None, None, &cancel(), deadline())
            .await,
        "us-east-1"
    );
}

#[test]
fn profile_selection_and_config_loading() {
    let empty = EnvSnapshot::test(&[]);
    assert_eq!(
        AwsCredentialResolver::resolve_profile_name(None, &empty),
        "default"
    );
    assert_eq!(
        AwsCredentialResolver::resolve_profile_name(Some("p"), &empty),
        "p"
    );
    let named = EnvSnapshot::test(&[("AWS_PROFILE", "env-profile")]);
    assert_eq!(
        AwsCredentialResolver::resolve_profile_name(None, &named),
        "env-profile"
    );
    assert!(!AwsCredentialResolver::should_load_shared_config(
        None, &empty
    ));
    assert!(AwsCredentialResolver::should_load_shared_config(
        Some("p"),
        &empty
    ));
    assert!(AwsCredentialResolver::should_load_shared_config(
        None, &named
    ));
    for value in ["1", "true", "True", "TRUE"] {
        let env = EnvSnapshot::test(&[("AWS_SDK_LOAD_CONFIG", value)]);
        assert!(
            AwsCredentialResolver::should_load_shared_config(None, &env),
            "{value}"
        );
    }
    for value in ["0", "false", "yes"] {
        let env = EnvSnapshot::test(&[("AWS_SDK_LOAD_CONFIG", value)]);
        assert!(
            !AwsCredentialResolver::should_load_shared_config(None, &env),
            "{value}"
        );
    }
}

#[tokio::test]
async fn availability_probe_mirrors_dispatch() {
    let dir = TestDir::new("probe");
    let disabled =
        EnvSnapshot::test(&[("AWS_EC2_METADATA_DISABLED", "true")]).with_cwd(dir.path().to_owned());
    assert!(
        !resolver(disabled, dir.path())
            .has_credential_source(None, &cancel(), deadline())
            .await
    );
    let keyed = EnvSnapshot::test(&[
        ("AWS_ACCESS_KEY_ID", "K"),
        ("AWS_SECRET_ACCESS_KEY", "S"),
        ("AWS_EC2_METADATA_DISABLED", "true"),
    ])
    .with_cwd(dir.path().to_owned());
    assert!(
        resolver(keyed, dir.path())
            .has_credential_source(None, &cancel(), deadline())
            .await
    );
    dir.write(
        ".aws/credentials",
        "[default]\naws_access_key_id = K\naws_secret_access_key = S\n",
    );
    let filed =
        EnvSnapshot::test(&[("AWS_EC2_METADATA_DISABLED", "true")]).with_cwd(dir.path().to_owned());
    assert!(
        resolver(filed, dir.path())
            .has_credential_source(None, &cancel(), deadline())
            .await
    );
}

#[test]
fn local_metadata_predicate_cases() {
    for host in [
        "localhost",
        "app.localhost",
        "metadata.google.internal",
        "127.0.0.1",
        "10.1.2.3",
        "192.168.0.1",
        "172.16.0.1",
        "172.31.255.255",
        "169.254.169.254",
        "169.254.170.2",
        "::1",
        "[fd00:ec2::254]",
        "[fe80::1]",
    ] {
        assert!(metadata::is_local_or_metadata_host(host), "{host}");
    }
    for host in [
        "example.com",
        "8.8.8.8",
        "192.0.2.1",
        "172.32.0.1",
        "172.15.255.255",
        "proxy.internal",
    ] {
        assert!(!metadata::is_local_or_metadata_host(host), "{host}");
    }
}

#[test]
fn region_labels_are_strict() {
    assert!(super::validate_region_label("us-east-1", "STS").is_ok());
    assert!(super::validate_region_label("cn-north-1", "SSO").is_ok());
    for bad in [
        "",
        "US-EAST-1",
        "us_east_1",
        "evil.com",
        "us-east-1 ",
        "a/b",
    ] {
        assert!(super::validate_region_label(bad, "STS").is_err(), "{bad}");
    }
}

#[test]
fn expiration_parsing_is_rfc3339_strict() {
    assert_eq!(
        super::parse_expiration_seconds("2030-01-01T00:00:00Z"),
        Some(1_893_456_000)
    );
    assert!(super::parse_expiration_seconds("2030-01-01T00:00:00.500Z").is_some());
    for bad in ["", "yesterday", "2030-13-01T00:00:00Z", "1893456000"] {
        assert!(super::parse_expiration_seconds(bad).is_none(), "{bad}");
    }
}

#[test]
fn sts_endpoint_construction() {
    let _ = profile::StsEndpoints::production("us-east-1").expect("us");
    let _ = profile::StsEndpoints::production("cn-north-1").expect("cn");
    assert!(profile::StsEndpoints::production("evil.com").is_err());
    let bad: url::Url = "https://example.com/".parse().expect("url");
    assert!(profile::StsEndpoints::loopback(&bad).is_err());
}
