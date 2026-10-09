//! `auth/status`, `auth/login`, and `auth/logout` for version-1 RPC.
//!
//! Every sign-in runs through [`Host::login`]. An OAuth login answers
//! `pending` with its URL (and device code) as soon as the flow reports one,
//! then keeps running inside the same handler future: the workspace forbids
//! `tokio::spawn`, so the waiter lives in the connection's task set. The
//! host publishes one `login_finished` update when the flow ends.

use std::sync::Arc;

use dal_agent::Host;
use dal_agent::login::{
    CredentialKind, LoginIo, LoginProgress, Method, StoredCredential, login_providers,
};
use serde::Serialize;
use sonic_rs::Value;
use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

use super::misc::route_identity;
use super::{Conn, host_error, id_key, invalid_params, opt_string, to_value};
use crate::jsonrpc::{ErrorObject, Id, Message};
use crate::transport::FrameWriter;

/// One `auth/status` provider row.
#[derive(Serialize)]
struct ProviderRow {
    provider: String,
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'static str>,
}

/// The `detail` word of a stored credential kind.
const fn kind_word(kind: CredentialKind) -> &'static str {
    match kind {
        CredentialKind::ApiKey => "api_key",
        CredentialKind::OAuth => "oauth",
    }
}

/// Handles `auth/status`: one row per sign-in provider, then one per other
/// catalog provider.
///
/// `expired` marks a stored OAuth credential inside the refresh window, the
/// same rule the refresher applies. `detail` names the stored credential kind
/// and is absent for a provider that is ready from the environment only.
pub(crate) async fn auth_status(host: &Host, params: &Value) -> Result<Value, ErrorObject> {
    let _ = params;
    let stored: Vec<StoredCredential> = host.stored_credentials().await.map_err(host_error)?;
    let mut names: Vec<String> = login_providers()
        .iter()
        .map(|def| def.id.to_owned())
        .collect();
    for info in host.models(None).await.map_err(host_error)? {
        let (_, provider) = route_identity(&info.route);
        if !names.contains(&provider) {
            names.push(provider);
        }
    }
    let mut rows: Vec<ProviderRow> = Vec::with_capacity(names.len());
    for provider in names {
        let entry = stored.iter().find(|row| *row.provider == *provider);
        let state = match entry {
            Some(row) if row.expired => "expired",
            _ if host.has_credential(&provider) => "ready",
            _ => "not_configured",
        };
        rows.push(ProviderRow {
            provider,
            state,
            detail: entry.map(|row| kind_word(row.kind)),
        });
    }
    rows.sort_by(|left, right| left.provider.as_bytes().cmp(right.provider.as_bytes()));
    let providers = to_value(&rows)?;
    Ok(sonic_rs::json!({"providers": providers}))
}

/// Parses a wire method word.
fn parse_method(word: &str) -> Option<Method> {
    [Method::ApiKey, Method::Browser, Method::Device]
        .into_iter()
        .find(|method| method.as_str() == word)
}

/// Checks that `provider` is a sign-in provider and offers `method`.
fn check_offered(provider: &str, method: Method) -> Result<(), ErrorObject> {
    let Some(def) = login_providers().into_iter().find(|def| def.id == provider) else {
        return Err(unknown_provider("auth/login", provider));
    };
    if def.offers(method) {
        return Ok(());
    }
    Err(invalid_params(
        "auth/login",
        format!("{provider} does not sign in with {method}"),
    ))
}

fn unknown_provider(method: &str, provider: &str) -> ErrorObject {
    invalid_params(method, format!(r#"unknown provider "{provider}""#))
}

/// Handles `auth/login`.
///
/// `api_key` stores the key and answers `ready`. `browser` and `device`
/// answer `pending` with the URL (and `userCode` for a device flow) and finish
/// through the `login_finished` host update. Once `pending` is sent the
/// request is finished: `$/cancel_request` no longer reaches it, and closing
/// the connection cancels the login.
pub(crate) async fn auth_login(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    writer: &FrameWriter,
    id: &Id,
    params: &Value,
) -> Option<Message> {
    let reply = match login(host, state, writer, id, params).await {
        Ok(Some(result)) => Message::Result {
            id: id.clone(),
            result,
        },
        Ok(None) => return None,
        Err(error) => Message::Error {
            id: id.clone(),
            error,
        },
    };
    Some(reply)
}

/// Runs one login; `Ok(None)` means the reply was already sent.
async fn login(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    writer: &FrameWriter,
    id: &Id,
    params: &Value,
) -> Result<Option<Value>, ErrorObject> {
    let provider = opt_string(params, "provider")
        .ok_or_else(|| invalid_params("auth/login", "missing member `provider`"))?;
    let word = opt_string(params, "method")
        .ok_or_else(|| invalid_params("auth/login", "missing member `method`"))?;
    let method = parse_method(&word)
        .ok_or_else(|| invalid_params("auth/login", format!(r#"unknown login method "{word}""#)))?;
    let key = match method {
        Method::ApiKey => Some(
            opt_string(params, "apiKey")
                .filter(|key| !key.is_empty())
                .ok_or_else(|| invalid_params("auth/login", r#"method "api_key" needs apiKey"#))?,
        ),
        Method::Browser | Method::Device => None,
    };
    check_offered(&provider, method)?;
    if let Some(key) = key {
        return store_key(host, &provider, key).await.map(Some);
    }
    let reply = Reply { state, writer, id };
    run_oauth(host, &reply, &provider, method).await
}

async fn store_key(host: &Host, provider: &str, key: String) -> Result<Value, ErrorObject> {
    let (sender, pasted) = oneshot::channel();
    sender
        .send(key)
        .map_err(|_| invalid_params("auth/login", "the key channel closed"))?;
    let (io, _progress) = LoginIo::channel(Some(pasted), CancellationToken::new());
    host.login(provider, Method::ApiKey, io)
        .await
        .map_err(host_error)?;
    Ok(sonic_rs::json!({"state": "ready"}))
}

/// The connection facts a login reply needs.
struct Reply<'a> {
    state: &'a Arc<Mutex<Conn>>,
    writer: &'a FrameWriter,
    id: &'a Id,
}

impl Reply<'_> {
    /// Answers `pending` and retires the request: it is finished, so
    /// `$/cancel_request` no longer reaches it and cannot add a second reply.
    async fn pending(&self, result: Value) {
        self.state.lock().await.inflight.remove(&id_key(self.id));
        super::send(
            self.writer,
            &Message::Result {
                id: self.id.clone(),
                result,
            },
        )
        .await;
    }
}

/// A running login registered with its connection, so closing the
/// connection cancels the flow. Dropping the slot cancels the login too.
struct LoginSlot {
    state: Arc<Mutex<Conn>>,
    key: String,
    cancel: CancellationToken,
}

impl LoginSlot {
    async fn register(state: &Arc<Mutex<Conn>>, key: String) -> Self {
        let cancel = CancellationToken::new();
        state
            .lock()
            .await
            .logins
            .insert(key.clone(), cancel.clone());
        Self {
            state: Arc::clone(state),
            key,
            cancel,
        }
    }

    async fn release(self) {
        self.state.lock().await.logins.remove(&self.key);
    }
}

impl Drop for LoginSlot {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Ok(mut locked) = self.state.try_lock() {
            locked.logins.remove(&self.key);
        }
    }
}

/// Drives an OAuth login: `pending` as soon as the flow shows a URL, then to
/// the end of the flow.
async fn run_oauth(
    host: &Host,
    reply: &Reply<'_>,
    provider: &str,
    method: Method,
) -> Result<Option<Value>, ErrorObject> {
    let slot = LoginSlot::register(reply.state, id_key(reply.id)).await;
    let (io, mut events) = LoginIo::channel(None, slot.cancel.clone());
    let login = host.login(provider, method, io);
    tokio::pin!(login);
    let mut replied = false;
    let outcome = loop {
        tokio::select! {
            biased;
            outcome = &mut login => {
                break match (outcome, replied) {
                    (Ok(_), false) => Ok(Some(sonic_rs::json!({"state": "ready"}))),
                    (Ok(_) | Err(_), true) => Ok(None),
                    (Err(error), false) => Err(host_error(error)),
                };
            }
            Some(event) = events.recv() => {
                let pending = match event {
                    LoginProgress::OpenUrl { url } => {
                        Some(sonic_rs::json!({"state": "pending", "url": url}))
                    }
                    LoginProgress::ShowCode { url, code } => {
                        Some(sonic_rs::json!({"state": "pending", "url": url, "userCode": code}))
                    }
                    LoginProgress::AskPaste { .. } | LoginProgress::Exchanging => None,
                };
                if let (Some(result), false) = (pending, replied) {
                    replied = true;
                    reply.pending(result).await;
                }
            }
        }
    };
    slot.release().await;
    outcome
}

/// Handles `auth/logout`: removes one provider's stored credential, or all.
pub(crate) async fn auth_logout(host: &Host, params: &Value) -> Result<Value, ErrorObject> {
    let provider = opt_string(params, "provider");
    if let Some(provider) = provider.as_deref()
        && !login_providers().iter().any(|def| def.id == provider)
    {
        return Err(unknown_provider("auth/logout", provider));
    }
    let removed = host.logout(provider.as_deref()).await.map_err(host_error)?;
    let removed: Vec<&str> = removed.iter().map(AsRef::as_ref).collect();
    Ok(sonic_rs::json!({"removed": to_value(&removed)?}))
}
