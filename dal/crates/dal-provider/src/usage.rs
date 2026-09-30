//! Luna Reserve: the Codex usage-error map, the account usage verdict, and
//! the per-host usage checker.
//!
//! Luna Reserve is the Codex model [`LUNA_RESERVE_MODEL`], shown as
//! [`LUNA_RESERVE_DISPLAY`], on the same Codex endpoint as the other Codex
//! models. The catalog owns its hidden row; this module only names the id.
//!
//! Nothing here switches a model or writes a journal record: the provider
//! reports verdicts and errors, and the loop decides. A verdict is never
//! guessed. An offer exists only when the backend body names this account
//! and user and carries a valid `rate_limit_upsell` banner of type
//! `luna_reserve`; every other shape is [`UsageVerdict::NoChange`] or a typed
//! [`UsageCheckReason`].
//!
//! Checker concurrency, exactly: one record per account id holds the last
//! outcome with its time and at most one in-flight request. Concurrent callers
//! for one account share that request. An outcome, success or error, is
//! reused for 5 s. A caller that drops its future ends only its own wait; the
//! request runs on the task the host spawned and stores its outcome before it
//! clears the in-flight marker. When the host drops that task, every waiter
//! gets [`UsageCheckReason::Transport`] and the marker is cleared, so the next
//! call starts a new request.

use std::collections::HashMap;
use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use dal_core::Family;
use futures::channel::oneshot;
use futures::future::{FutureExt, Shared};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use url::Url;

use crate::auth::credential::{Credential, SecretString, codex_identity};
use crate::error::{ProviderError, UsageCheckReason};
use crate::http::{Exchange, USAGE_TIMEOUT, endpoint, read_body, send};

/// The Codex model id of Luna Reserve.
pub const LUNA_RESERVE_MODEL: &str = "gpt-reserve";
/// The display name of Luna Reserve.
pub const LUNA_RESERVE_DISPLAY: &str = "Luna Reserve";

/// The opt-in header the usage read sends so the backend includes the
/// Luna Reserve banner.
pub(crate) const RESERVE_HEADER: (&str, &str) = ("x-openai-codex-luna-reserve", "1");

/// How long one usage outcome is reused for the same account.
pub(crate) const CACHE_TTL: Duration = Duration::from_secs(5);

const NO_REASON: &str = "the server gave no reason";
const CANCELLED: &str = "the usage request was cancelled";
const MESSAGE_LINE_LIMIT: usize = 300;

const TITLE_BYTES: usize = 1024;
const TITLE_LINES: usize = 3;
const DESCRIPTION_BYTES: usize = 4096;
const DESCRIPTION_LINES: usize = 12;
const MAX_CTAS: usize = 8;
const SLUG_BYTES: usize = 256;
const MAX_FALLBACK_SLUGS: usize = 16;
const LABEL_BYTES: usize = 256;

const BILLING_URL: &str =
    "https://chatgpt.com/admin/billing?codex_credit_action=add_credits&account_id=";
const CREDITS_URL: &str = "https://chatgpt.com/codex/settings/usage?credits_modal=true";
const PRO_URL: &str = "https://chatgpt.com/?cta_tab=personal&highlight_plan=pro#pricing";
const PRO_2X_URL: &str =
    "https://chatgpt.com/?cta_tab=personal&highlight_plan=pro&pro_variant=2x#pricing";
const PLUS_URL: &str = "https://chatgpt.com/?cta_tab=personal&highlight_plan=plus#pricing";
const USAGE_URL: &str = "https://chatgpt.com/codex/settings/usage";

/// Plans whose credit purchase goes through the workspace admin billing page.
const WORKSPACE_PLANS: [&str; 12] = [
    "team",
    "self_serve_business_prolite",
    "self_serve_business_usage_based",
    "business",
    "ent26",
    "enterprise_cbp_automation",
    "enterprise_cbp_usage_based",
    "enterprise",
    "edu",
    "education",
    "edu_plus",
    "edu_pro",
];

/// `rate_limit_reached_type.type` values that keep ordinary usage blocked.
const BLOCKING_REACHED_TYPES: [&str; 5] = [
    "rate_limit_reached",
    "workspace_owner_credits_depleted",
    "workspace_member_credits_depleted",
    "workspace_owner_usage_limit_reached",
    "workspace_member_usage_limit_reached",
];

/// One purchase or plan action of an offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cta {
    /// The backend's label text.
    pub label: String,
    /// The web page dalgon shows for the action.
    pub url: String,
}

/// A Luna Reserve offer: ordinary usage is blocked and Luna Reserve is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// Backend copy that is true only after the session uses Luna Reserve.
    pub after_switch_title: String,
    /// Backend copy shown under [`Offer::after_switch_title`].
    pub after_switch_description: String,
    /// At most 8 actions, in backend order.
    pub ctas: Vec<Cta>,
    /// The model id the backend blocked, when it names one.
    pub blocked_model: Option<String>,
    /// The model whose settings Luna Reserve borrows, when the backend names one.
    pub normal_model: Option<String>,
}

/// The verdict of one account usage read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageVerdict {
    /// Luna Reserve is open for this account.
    ReserveOffered(Offer),
    /// The backend allows ordinary usage again.
    OrdinaryUsageBack,
    /// The read proves neither.
    NoChange,
}

/// The outcome of one usage read.
pub(crate) type UsageOutcome = Result<UsageVerdict, UsageCheckReason>;

/// A request task handed to the host's task set.
pub(crate) type UsageTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Maps a Codex error reply onto a Luna Reserve or usage error.
///
/// `body` is the raw HTTP error body or the whole WebSocket
/// `{"type":"error","status":429,...}` frame; both carry the error object in a
/// top-level `error` member. `model` is the provider model id of the request.
/// A 429 whose `error.type` is `usage_limit_reached` gives
/// [`ProviderError::UsageLimit`]; `usage_not_included` gives
/// [`ProviderError::UsageNotIncluded`]; a 400, 403, or 404 for
/// [`LUNA_RESERVE_MODEL`] gives [`ProviderError::ReserveUnavailable`]. Every
/// other reply is `None`, and the request lifecycle keeps its own mapping.
/// None of these errors is retryable.
#[must_use]
pub(crate) fn map_codex_error(status: u16, body: &str, model: &str) -> Option<ProviderError> {
    let value = sonic_rs::from_str::<Value>(body).ok();
    let value = value.as_ref();
    match status {
        429 => {
            let kind = value
                .and_then(|value| value.get("error"))
                .and_then(|error| error.get("type"))
                .and_then(JsonValueTrait::as_str);
            match kind {
                Some("usage_limit_reached") => Some(ProviderError::UsageLimit {
                    model: model.to_owned(),
                    message: server_message(value),
                }),
                Some("usage_not_included") => Some(ProviderError::UsageNotIncluded {
                    message: server_message(value),
                }),
                _ => None,
            }
        }
        400 | 403 | 404 if model == LUNA_RESERVE_MODEL => {
            tracing::info!(target: "dalgon.provider", "codex rejected gpt-reserve with status {status}");
            Some(ProviderError::ReserveUnavailable {
                status,
                message: server_message(value),
            })
        }
        _ => None,
    }
}

/// The server message of an error body: the trimmed `error.message` when not
/// empty, else the trimmed top-level `detail` when not empty, else a fixed
/// sentence; then its first line, cut at 300 bytes on a UTF-8 boundary.
fn server_message(value: Option<&Value>) -> String {
    let error_message = value
        .and_then(|value| value.get("error"))
        .and_then(|error| error.get("message"));
    let text = non_blank(error_message)
        .or_else(|| non_blank(value.and_then(|value| value.get("detail"))))
        .unwrap_or(NO_REASON);
    first_line(text).to_owned()
}

fn non_blank(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(JsonValueTrait::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn first_line(text: &str) -> &str {
    let end = text.find(['\n', '\r']).unwrap_or(text.len());
    let line = &text[..end];
    if line.len() <= MESSAGE_LINE_LIMIT {
        return line;
    }
    let mut cut = MESSAGE_LINE_LIMIT;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    &line[..cut]
}

/// Decodes the usage body of account `account_id` and user `user_id`.
///
/// Unknown members are ignored. The body must name exactly this account and
/// this user; another account, another user, or an unknown user gives
/// [`UsageVerdict::NoChange`]. Only the snake-case `rate_limit_upsell` member
/// is read.
///
/// # Errors
///
/// [`UsageCheckReason::NotJsonObject`] when `body` is not one JSON object.
pub(crate) fn decode_verdict(account_id: &str, user_id: Option<&str>, body: &str) -> UsageOutcome {
    let value = sonic_rs::from_str::<Value>(body).map_err(|_| UsageCheckReason::NotJsonObject)?;
    if !value.is_object() {
        return Err(UsageCheckReason::NotJsonObject);
    }
    let Some(user_id) = user_id else {
        return Ok(UsageVerdict::NoChange);
    };
    let names = |key: &str, expected: &str| {
        value.get(key).and_then(JsonValueTrait::as_str) == Some(expected)
    };
    if !names("account_id", account_id) || !names("user_id", user_id) {
        return Ok(UsageVerdict::NoChange);
    }
    match value.get("rate_limit_upsell") {
        Some(upsell) if !upsell.is_null() => Ok(banner(upsell)
            .filter(|banner| banner.kind == "luna_reserve")
            .map_or(UsageVerdict::NoChange, |banner| {
                UsageVerdict::ReserveOffered(offer(&banner, &value, account_id))
            })),
        _ if ordinary_back(&value) => Ok(UsageVerdict::OrdinaryUsageBack),
        _ => Ok(UsageVerdict::NoChange),
    }
}

/// A banner that passed every validity rule.
struct Banner<'a> {
    kind: &'a str,
    title: &'a str,
    description: &'a str,
    ctas: Vec<(&'a str, &'a str)>,
    blocked_model: Option<&'a str>,
}

/// Parses and validates a `rate_limit_upsell` banner; `None` when invalid.
fn banner(upsell: &Value) -> Option<Banner<'_>> {
    let kind = upsell.get("banner_type")?.as_str()?;
    let title = upsell.get("title")?.as_str()?;
    let description = upsell.get("description")?.as_str()?;
    let ctas = upsell
        .get("ctas")?
        .as_array()?
        .iter()
        .map(|entry| {
            Some((
                entry.get("action")?.as_str()?,
                entry.get("label")?.as_str()?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    if let Some(presentation) = upsell.get("presentation")
        && !matches!(presentation.as_str(), Some("inline" | "dismissible"))
    {
        return None;
    }
    if title.len() > TITLE_BYTES || title.trim().is_empty() || line_count(title) > TITLE_LINES {
        return None;
    }
    if description.len() > DESCRIPTION_BYTES || line_count(description) > DESCRIPTION_LINES {
        return None;
    }
    if ctas.len() > MAX_CTAS {
        return None;
    }
    let blocked_model = match upsell.get("blocked_model_slug") {
        Some(slug) if !slug.is_null() => Some(slug.as_str().filter(|slug| valid_slug(slug))?),
        _ => None,
    };
    if let Some(fallbacks) = upsell
        .get("fallback_model_slugs")
        .filter(|value| !value.is_null())
    {
        let fallbacks = fallbacks.as_array()?;
        let valid = fallbacks
            .iter()
            .all(|slug| slug.as_str().is_some_and(valid_slug));
        if fallbacks.len() > MAX_FALLBACK_SLUGS || !valid {
            return None;
        }
    }
    Some(Banner {
        kind,
        title,
        description,
        ctas,
        blocked_model,
    })
}

/// The number of `\n`-separated pieces, not counting an empty piece after a
/// final `\n`.
fn line_count(text: &str) -> usize {
    let pieces = text.split('\n').count();
    if text.ends_with('\n') {
        pieces - 1
    } else {
        pieces
    }
}

fn valid_slug(slug: &str) -> bool {
    !slug.trim().is_empty() && slug.len() <= SLUG_BYTES && !slug.chars().any(char::is_control)
}

/// Whether a body without an upsell proves that ordinary usage is allowed.
fn ordinary_back(body: &Value) -> bool {
    let Some(allowed) = body
        .get("rate_limit")
        .and_then(|limit| limit.get("allowed"))
        .and_then(JsonValueTrait::as_bool)
    else {
        return false;
    };
    let credits = body.get("credits");
    let permitted = allowed || is_true(credits, "has_credits") || is_true(credits, "unlimited");
    let blocked_type = body
        .get("rate_limit_reached_type")
        .and_then(|reached| reached.get("type"))
        .and_then(JsonValueTrait::as_str)
        .is_some_and(|kind| BLOCKING_REACHED_TYPES.contains(&kind));
    permitted && !is_true(body.get("spend_control"), "reached") && !blocked_type
}

fn is_true(object: Option<&Value>, key: &str) -> bool {
    object
        .and_then(|object| object.get(key))
        .and_then(JsonValueTrait::as_bool)
        == Some(true)
}

fn offer(banner: &Banner<'_>, body: &Value, account_id: &str) -> Offer {
    let plan = body.get("plan_type").and_then(JsonValueTrait::as_str);
    Offer {
        after_switch_title: strip_controls(banner.title),
        after_switch_description: strip_controls(banner.description),
        ctas: banner
            .ctas
            .iter()
            .filter_map(|&(action, label)| cta(action, label, plan, account_id))
            .collect(),
        blocked_model: banner.blocked_model.map(str::to_owned),
        normal_model: normal_model(body),
    }
}

/// Removes every control character except `\n`.
fn strip_controls(text: &str) -> String {
    text.chars()
        .filter(|&character| character == '\n' || !character.is_control())
        .collect()
}

fn cta(action: &str, label: &str, plan: Option<&str>, account_id: &str) -> Option<Cta> {
    if label.trim().is_empty() || label.len() > LABEL_BYTES || label.chars().any(char::is_control) {
        return None;
    }
    let url = match action {
        "add_credits" | "buy_credits" => {
            if plan.is_some_and(|plan| WORKSPACE_PLANS.contains(&plan)) {
                let account: String =
                    url::form_urlencoded::byte_serialize(account_id.as_bytes()).collect();
                format!("{BILLING_URL}{account}")
            } else {
                CREDITS_URL.to_owned()
            }
        }
        "open_pricing_dialog" => match plan {
            Some("plus") => PRO_URL,
            Some("prolite") => PRO_2X_URL,
            _ => PLUS_URL,
        }
        .to_owned(),
        "view_usage" => USAGE_URL.to_owned(),
        _ => return None,
    };
    Some(Cta {
        label: label.to_owned(),
        url,
    })
}

/// The `normal_model_slug` of the first `additional_rate_limits` entry named
/// [`LUNA_RESERVE_MODEL`], when it is a non-empty string.
fn normal_model(body: &Value) -> Option<String> {
    body.get("additional_rate_limits")?
        .as_array()?
        .iter()
        .find(|entry| {
            entry.get("limit_name").and_then(JsonValueTrait::as_str) == Some(LUNA_RESERVE_MODEL)
        })?
        .get("normal_model_slug")?
        .as_str()
        .filter(|slug| !slug.is_empty())
        .map(str::to_owned)
}

type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;
type Pending = Shared<oneshot::Receiver<UsageOutcome>>;

#[derive(Default)]
struct State {
    records: HashMap<String, Record>,
    next_request: u64,
}

#[derive(Default)]
struct Record {
    last: Option<(Instant, UsageOutcome)>,
    inflight: Option<(u64, Pending)>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The account usage reader one host shares across its sessions.
pub(crate) struct UsageChecker {
    client: reqwest::Client,
    url: Url,
    user_agent: Arc<str>,
    timeout: Duration,
    clock: Clock,
    state: Arc<Mutex<State>>,
}

impl fmt::Debug for UsageChecker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UsageChecker")
            .field("url", &self.url.as_str())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl UsageChecker {
    /// A checker that reads `<codex_base minus a trailing /codex>/wham/usage`
    /// through `client`, sending `user_agent`.
    ///
    /// # Errors
    ///
    /// Every [`endpoint`] failure of the derived usage URL.
    pub(crate) fn new(
        client: reqwest::Client,
        codex_base: &str,
        user_agent: &str,
    ) -> Result<Self, ProviderError> {
        Self::with_timing(
            client,
            codex_base,
            user_agent,
            USAGE_TIMEOUT,
            Arc::new(Instant::now),
        )
    }

    fn with_timing(
        client: reqwest::Client,
        codex_base: &str,
        user_agent: &str,
        timeout: Duration,
        clock: Clock,
    ) -> Result<Self, ProviderError> {
        let base = codex_base.trim_end_matches('/');
        let base = base.strip_suffix("/codex").unwrap_or(base);
        Ok(Self {
            client,
            url: endpoint(Family::Codex, base, "wham/usage")?,
            user_agent: Arc::from(user_agent),
            timeout,
            clock,
            state: Arc::default(),
        })
    }

    /// Reads the usage verdict of the Codex sign-in in `credential`.
    ///
    /// An API key, a credential without a decodable Codex ID token, and a
    /// `FedRAMP` account give `Ok(NoChange)` with no request and no task. An
    /// outcome younger than 5 s is returned again. Otherwise the caller joins
    /// the account's in-flight request, or starts one by handing its task to
    /// `spawn` exactly once. Dropping the returned future ends only this wait.
    pub(crate) fn check(
        &self,
        credential: &Credential,
        spawn: &dyn Fn(UsageTask),
    ) -> impl Future<Output = UsageOutcome> + Send + use<> {
        let waiter = self.begin(credential, spawn);
        async move {
            match waiter {
                Waiter::Ready(outcome) => outcome,
                Waiter::Pending(pending) => pending.await.unwrap_or_else(|_| {
                    Err(UsageCheckReason::Transport {
                        reason: CANCELLED.to_owned(),
                    })
                }),
            }
        }
    }

    fn begin(&self, credential: &Credential, spawn: &dyn Fn(UsageTask)) -> Waiter {
        let Credential::OAuth(oauth) = credential else {
            return Waiter::Ready(Ok(UsageVerdict::NoChange));
        };
        let Some(identity) = oauth.id_token.as_deref().and_then(codex_identity) else {
            return Waiter::Ready(Ok(UsageVerdict::NoChange));
        };
        if identity.fedramp {
            return Waiter::Ready(Ok(UsageVerdict::NoChange));
        }
        let now = (self.clock)();
        let mut state = lock(&self.state);
        let State {
            records,
            next_request,
        } = &mut *state;
        let record = records.entry(identity.account_id.clone()).or_default();
        if let Some((at, outcome)) = &record.last
            && now.saturating_duration_since(*at) < CACHE_TTL
        {
            return Waiter::Ready(outcome.clone());
        }
        if let Some((_, pending)) = &record.inflight {
            return Waiter::Pending(pending.clone());
        }
        let id = *next_request;
        *next_request = next_request.wrapping_add(1);
        let (sender, receiver) = oneshot::channel();
        let pending = receiver.shared();
        record.inflight = Some((id, pending.clone()));
        drop(state);

        let mut secrets = vec![oauth.access_token.clone(), oauth.refresh_token.clone()];
        secrets.extend(oauth.id_token.as_deref().map(SecretString::from));
        let fetch = Fetch {
            client: self.client.clone(),
            url: self.url.clone(),
            user_agent: Arc::clone(&self.user_agent),
            timeout: self.timeout,
            token: oauth.access_token.clone(),
            account_id: identity.account_id.clone(),
            user_id: identity.user_id,
            secrets,
        };
        let guard = InflightGuard {
            state: Arc::clone(&self.state),
            account_id: identity.account_id,
            id,
        };
        let clock = Arc::clone(&self.clock);
        spawn(Box::pin(async move {
            let outcome = fetch.run().await;
            guard.store(clock(), outcome.clone());
            drop(guard);
            sender.send(outcome).ok();
        }));
        Waiter::Pending(pending)
    }
}

enum Waiter {
    Ready(UsageOutcome),
    Pending(Pending),
}

/// Clears its request's in-flight marker when the request task ends or is
/// dropped, even before its first poll.
struct InflightGuard {
    state: Arc<Mutex<State>>,
    account_id: String,
    id: u64,
}

impl InflightGuard {
    fn store(&self, at: Instant, outcome: UsageOutcome) {
        let mut state = lock(&self.state);
        if let Some(record) = state.records.get_mut(&self.account_id) {
            record.last = Some((at, outcome));
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut state = lock(&self.state);
        if let Some(record) = state.records.get_mut(&self.account_id)
            && record
                .inflight
                .as_ref()
                .is_some_and(|(id, _)| *id == self.id)
        {
            record.inflight = None;
        }
    }
}

/// One usage request with everything it needs owned.
struct Fetch {
    client: reqwest::Client,
    url: Url,
    user_agent: Arc<str>,
    timeout: Duration,
    token: SecretString,
    account_id: String,
    user_id: Option<String>,
    secrets: Vec<SecretString>,
}

impl Fetch {
    /// One attempt bounded by the timeout; status 200 is decoded, any other
    /// status is an error carrying the redacted body.
    async fn run(self) -> UsageOutcome {
        let request = self
            .client
            .get(self.url.clone())
            .bearer_auth(self.token.expose())
            .header("chatgpt-account-id", self.account_id.as_str())
            .header(RESERVE_HEADER.0, RESERVE_HEADER.1);
        let started = tokio::time::Instant::now();
        let exchange = Exchange::Json {
            total: self.timeout,
        };
        let read = async {
            let response = send(
                Family::Codex,
                request,
                &self.user_agent,
                exchange,
                tokio::time::sleep,
            )
            .await?;
            let status = response.status().as_u16();
            let body = read_body(Family::Codex, response).await?;
            Ok::<_, ProviderError>((status, body))
        };
        match tokio::time::timeout(self.timeout, read).await {
            Err(_) => Err(UsageCheckReason::Timeout),
            Ok(Err(_)) if started.elapsed() >= self.timeout => Err(UsageCheckReason::Timeout),
            Ok(Err(error)) => {
                let reason = match error {
                    ProviderError::Transport { reason, .. } => reason,
                    other => other.to_string(),
                };
                Err(UsageCheckReason::Transport {
                    reason: self.redact(&reason),
                })
            }
            Ok(Ok((200, body))) => decode_verdict(
                &self.account_id,
                self.user_id.as_deref(),
                &String::from_utf8_lossy(&body),
            ),
            Ok(Ok((status, body))) => Err(UsageCheckReason::Status {
                status,
                message: self.redact(&String::from_utf8_lossy(&body)),
            }),
        }
    }

    /// Replaces every echo of a credential secret.
    fn redact(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for secret in &self.secrets {
            let secret = secret.expose();
            if !secret.is_empty() {
                text = text.replace(secret, "<redacted>");
            }
        }
        text
    }
}

#[cfg(test)]
mod tests;
