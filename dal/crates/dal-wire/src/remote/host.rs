//! Remote host operations over version-1 methods.

use std::collections::VecDeque;
use std::sync::Arc;

use dal_agent::SessionRef;
use dal_core::{
    CommandName, CommandSpec, GenerationId, ListQuery, ModelRequest, ModelRoute, Page, SessionId,
    SessionInfo,
};
use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait, Value};

use super::agent::RemoteAgent;
use super::conn::Shared;
use super::decode::{decode, encode, malformed, opt_string, session_id, string};
use super::{
    CancellableLogin, RemoteAuthRow, RemoteEndpoint, RemoteHostUpdate, RemoteLogin,
    RemoteLoginMethod, RemoteModel,
};
use crate::error::WireError;

/// A remote host speaking the version-1 protocol.
#[derive(Clone)]
pub struct RemoteHost {
    /// The shared connection.
    shared: Arc<Shared>,
}

/// A remote host lifecycle subscription.
///
/// Dropping it stops local delivery only.
pub struct RemoteHostSubscription {
    /// The shared connection.
    shared: Arc<Shared>,
    /// Identifies this handle's subscription slot.
    fence: u64,
}

impl RemoteHost {
    /// Connects to one endpoint and runs `initialize`.
    ///
    /// The first connection is attempted once; later connection loss is
    /// retried with backoff by whichever caller is waiting.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the endpoint cannot be reached or the
    /// server refuses the handshake.
    pub async fn connect(endpoint: RemoteEndpoint) -> Result<Self, WireError> {
        Ok(Self {
            shared: Shared::connect(endpoint, None).await?,
        })
    }

    /// Connects to a public WebSocket endpoint using a bearer token.
    ///
    /// The token is sent only in the WebSocket upgrade's authorization header
    /// and is retained only so a later reconnect can authenticate again.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when authentication, connection, or initialization fails.
    pub async fn connect_with_auth(
        endpoint: RemoteEndpoint,
        token: &str,
    ) -> Result<Self, WireError> {
        if matches!(&endpoint, RemoteEndpoint::LocalSocket(_)) {
            return Err(WireError::Transport(
                "bearer authentication requires a WebSocket endpoint".to_owned(),
            ));
        }
        Ok(Self {
            shared: Shared::connect(endpoint, Some(Arc::from(token))).await?,
        })
    }

    /// Lists sessions through `session/list`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails or the page does not decode.
    pub async fn sessions(
        &self,
        query: &ListQuery,
    ) -> Result<Page<SessionInfo, Box<str>>, WireError> {
        let mut params = sonic_rs::json!({});
        if let Some(object) = params.as_object_mut() {
            if let Some(limit) = query.limit {
                object.insert("limit", Value::from(limit));
            }
            if let Some(cursor) = query.cursor.as_deref() {
                object.insert("cursor", Value::from(cursor));
            }
            if let Some(search) = query.search.as_deref() {
                object.insert("search", Value::from(search));
            }
        }
        let result = self.shared.call("session/list", params).await?;
        let sessions = result
            .get("sessions")
            .ok_or_else(|| malformed("sessions"))?;
        Ok(Page {
            items: decode(sessions, "sessions")?,
            next_before: opt_string(&result, "nextCursor").map(String::into_boxed_str),
        })
    }

    /// Opens one session through `session/open`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails; a session held by another
    /// process keeps its stable `-32008` error.
    pub async fn open(&self, session: SessionRef) -> Result<RemoteAgent, WireError> {
        let params = sonic_rs::json!({"ref": encode(&session)?});
        let result = self.shared.call("session/open", params).await?;
        Ok(RemoteAgent {
            shared: Arc::clone(&self.shared),
            session: session_id(&result, "sessionId")?,
        })
    }

    /// Closes one session through `session/close`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn close(&self, id: SessionId) -> Result<(), WireError> {
        self.shared
            .call(
                "session/close",
                sonic_rs::json!({"sessionId": id.to_string()}),
            )
            .await?;
        Ok(())
    }

    /// Lists the merged command table through `commands/list`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails or a row does not decode.
    pub async fn commands(&self) -> Result<Arc<[CommandSpec]>, WireError> {
        let result = self
            .shared
            .call("commands/list", sonic_rs::json!({}))
            .await?;
        rows(&result, "commands")?
            .iter()
            .map(|row| {
                let name = string(row, "name")?;
                Ok(CommandSpec {
                    name: CommandName::parse(name).map_err(|error| WireError::Protocol {
                        code: -32700,
                        message: format!("remote frame has an invalid command name: {error}"),
                    })?,
                    summary: string(row, "summary")?.into(),
                    args_hint: opt_string(row, "args").map(String::into_boxed_str),
                })
            })
            .collect()
    }

    /// Lists display models through `models/list`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails or a row does not decode.
    pub async fn models(&self) -> Result<Vec<RemoteModel>, WireError> {
        let result = self.shared.call("models/list", sonic_rs::json!({})).await?;
        rows(&result, "models")?
            .iter()
            .map(|row| {
                let context_window = match row.get("contextWindow") {
                    None => None,
                    Some(value) => Some(
                        value
                            .as_u64()
                            .and_then(|window| u32::try_from(window).ok())
                            .ok_or_else(|| malformed("contextWindow"))?,
                    ),
                };
                Ok(RemoteModel {
                    id: string(row, "id")?.to_owned(),
                    provider: string(row, "provider")?.to_owned(),
                    name: string(row, "name")?.to_owned(),
                    context_window,
                })
            })
            .collect()
    }

    /// Reads every provider's sign-in state through `auth/status`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails or a row is malformed.
    pub async fn auth_status(&self) -> Result<Vec<RemoteAuthRow>, WireError> {
        let result = self.shared.call("auth/status", sonic_rs::json!({})).await?;
        rows(&result, "providers")?
            .iter()
            .map(|row| {
                Ok(RemoteAuthRow {
                    provider: string(row, "provider")?.to_owned(),
                    state: string(row, "state")?.to_owned(),
                    detail: opt_string(row, "detail"),
                })
            })
            .collect()
    }

    /// Reads one document's text through `docs/read`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn doc(&self, uri: &str) -> Result<String, WireError> {
        let result = self
            .shared
            .call("docs/read", sonic_rs::json!({"uri": uri}))
            .await?;
        Ok(string(&result, "text")?.to_owned())
    }

    /// Logs in to one provider through `auth/login`.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn login(
        &self,
        provider: &str,
        method: RemoteLoginMethod,
    ) -> Result<RemoteLogin, WireError> {
        let result = self
            .shared
            .call("auth/login", Self::login_params(provider, method))
            .await?;
        match string(&result, "state")? {
            "ready" => Ok(RemoteLogin::Ready),
            "pending" => Ok(RemoteLogin::Pending {
                url: string(&result, "url")?.to_owned(),
                user_code: opt_string(&result, "userCode"),
            }),
            _ => Err(malformed("state")),
        }
    }

    /// Logs in through `auth/login`, keeping the pending attempt's cancel id.
    ///
    /// An API-key login finishes at once and answers
    /// [`CancellableLogin::Ready`]; a browser or device login answers
    /// [`CancellableLogin::Pending`] with the `loginId` to pass to
    /// [`Self::cancel_login`].
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn login_cancellable(
        &self,
        provider: &str,
        method: RemoteLoginMethod,
    ) -> Result<CancellableLogin, WireError> {
        let result = self
            .shared
            .call("auth/login", Self::login_params(provider, method))
            .await?;
        match string(&result, "state")? {
            "ready" => Ok(CancellableLogin::Ready),
            "pending" => Ok(CancellableLogin::Pending {
                login_id: result
                    .get("loginId")
                    .and_then(JsonValueTrait::as_u64)
                    .ok_or_else(|| malformed("loginId"))?,
                url: string(&result, "url")?.to_owned(),
                user_code: opt_string(&result, "userCode"),
            }),
            _ => Err(malformed("state")),
        }
    }

    /// Cancels a pending login through `auth/cancel`.
    ///
    /// Returns whether a pending attempt was cancelled: false when the id is
    /// unknown or its login already finished.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn cancel_login(&self, login_id: u64) -> Result<bool, WireError> {
        let result = self
            .shared
            .call("auth/cancel", sonic_rs::json!({"loginId": login_id}))
            .await?;
        result
            .get("cancelled")
            .and_then(JsonValueTrait::as_bool)
            .ok_or_else(|| malformed("cancelled"))
    }

    /// Builds the `auth/login` params for one method.
    fn login_params(provider: &str, method: RemoteLoginMethod) -> Value {
        match method {
            RemoteLoginMethod::ApiKey(key) => {
                sonic_rs::json!({"provider": provider, "method": "api_key", "apiKey": key})
            }
            RemoteLoginMethod::Browser => {
                sonic_rs::json!({"provider": provider, "method": "browser"})
            }
            RemoteLoginMethod::Device => {
                sonic_rs::json!({"provider": provider, "method": "device"})
            }
        }
    }

    /// Removes stored credentials through `auth/logout`; `None` removes all.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn logout(&self, provider: Option<&str>) -> Result<Vec<String>, WireError> {
        let params = match provider {
            Some(provider) => sonic_rs::json!({"provider": provider}),
            None => sonic_rs::json!({}),
        };
        let result = self.shared.call("auth/logout", params).await?;
        decode(
            result.get("removed").ok_or_else(|| malformed("removed"))?,
            "removed",
        )
    }

    /// Subscribes to host lifecycle updates through `host/subscribe`.
    ///
    /// After a reconnect the subscription is renewed and delivers
    /// [`RemoteHostUpdate::Reconnected`].
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the call fails.
    pub async fn subscribe(&self) -> Result<RemoteHostSubscription, WireError> {
        let fence = {
            let mut state = self.shared.core.state();
            let fence = state.next_fence();
            state.host_sub = Some((fence, VecDeque::new()));
            fence
        };
        let subscription = RemoteHostSubscription {
            shared: Arc::clone(&self.shared),
            fence,
        };
        self.shared
            .call("host/subscribe", sonic_rs::json!({}))
            .await?;
        Ok(subscription)
    }

    /// Provider relay has no version-1 method.
    ///
    /// # Errors
    ///
    /// Always returns [`WireError::Unsupported`].
    pub fn relay(
        &self,
        _route: &ModelRoute,
        _request: &ModelRequest,
    ) -> Result<dal_provider::EventStream, WireError> {
        Err(WireError::Unsupported { operation: "relay" })
    }

    /// The extension generation has no version-1 method.
    ///
    /// # Errors
    ///
    /// Always returns [`WireError::Unsupported`].
    pub fn generation(&self) -> Result<GenerationId, WireError> {
        Err(WireError::Unsupported {
            operation: "generation",
        })
    }
}

impl RemoteHostSubscription {
    /// Returns the next host update. Cancel safe.
    ///
    /// # Errors
    ///
    /// Returns [`WireError`] when the connection fails with a non-transport
    /// error or a newer host subscription replaced this one.
    pub async fn next(&mut self) -> Result<RemoteHostUpdate, WireError> {
        let fence = self.fence;
        self.shared
            .wait(|state| match state.host_sub.as_mut() {
                Some((live, queue)) if *live == fence => queue.pop_front().map(Ok),
                _ => Some(Err(WireError::Transport(
                    "host subscription was replaced".to_owned(),
                ))),
            })
            .await
    }
}

impl Drop for RemoteHostSubscription {
    fn drop(&mut self) {
        let mut state = self.shared.core.state();
        if state
            .host_sub
            .as_ref()
            .is_some_and(|(live, _)| *live == self.fence)
        {
            state.host_sub = None;
        }
    }
}

/// Reads one required array member.
fn rows<'a>(result: &'a Value, name: &str) -> Result<&'a sonic_rs::Array, WireError> {
    result
        .get(name)
        .and_then(|value| value.as_array())
        .ok_or_else(|| malformed(name))
}
