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
        rows(&result, "models")?.iter().map(model_row).collect()
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

/// Decodes one `models/list` row.
///
/// `route` and `reference` are additive members: an absent member means the
/// server predates it and the row decodes with `None`, which keeps the row
/// usable under the old selection rules. A member that is present but of the
/// wrong shape (`null`, a non-string reference, a route that does not decode)
/// is refused, never read as absent.
fn model_row(row: &Value) -> Result<RemoteModel, WireError> {
    let context_window = match row.get("contextWindow") {
        None => None,
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|window| u32::try_from(window).ok())
                .ok_or_else(|| malformed("contextWindow"))?,
        ),
    };
    let route = match row.get("route") {
        None => None,
        Some(value) => Some(decode::<ModelRoute>(value, "route")?),
    };
    let reference = match row.get("reference") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| malformed("reference"))?
                .to_owned(),
        ),
    };
    Ok(RemoteModel {
        id: string(row, "id")?.to_owned(),
        provider: string(row, "provider")?.to_owned(),
        name: string(row, "name")?.to_owned(),
        context_window,
        route,
        reference,
    })
}

/// Reads one required array member.
fn rows<'a>(result: &'a Value, name: &str) -> Result<&'a sonic_rs::Array, WireError> {
    result
        .get(name)
        .and_then(|value| value.as_array())
        .ok_or_else(|| malformed(name))
}

#[cfg(test)]
mod tests {
    use dal_core::{Family, ModelRoute};
    use sonic_rs::Value;

    use super::model_row;
    use crate::error::WireError;
    use crate::remote::RemoteModel;

    /// A `models/list` row with the legacy members and `members` appended.
    fn row(members: &str) -> Value {
        let text = format!(
            r#"{{"id":"gpt-4o","provider":"openai","name":"GPT-4o","contextWindow":128000{members}}}"#
        );
        sonic_rs::from_str(&text).expect("row json")
    }

    fn legacy(route: Option<ModelRoute>, reference: Option<&str>) -> RemoteModel {
        RemoteModel {
            id: "gpt-4o".into(),
            provider: "openai".into(),
            name: "GPT-4o".into(),
            context_window: Some(128_000),
            route,
            reference: reference.map(str::to_owned),
        }
    }

    /// Asserts that `members` makes the row a refused `-32700` protocol error
    /// naming `what`.
    #[track_caller]
    fn refused(members: &str, what: &str) {
        match model_row(&row(members)) {
            Err(WireError::Protocol {
                code: -32700,
                message,
            }) => assert!(
                message.starts_with(&format!("remote frame has an invalid {what}")),
                "{members}: {message}"
            ),
            other => panic!("{members} must be refused as an invalid {what}, got {other:?}"),
        }
    }

    #[test]
    fn a_row_from_a_server_that_predates_routes_decodes_without_them() {
        let decoded = model_row(&row("")).expect("an old row decodes");
        assert_eq!(decoded, legacy(None, None));
    }

    #[test]
    fn a_current_row_decodes_its_route_and_reference() {
        let members = concat!(
            r#","route":{"kind":"api","family":"openai_responses","#,
            r#""model":"gpt-4o","provider":"openrouter"},"reference":"openrouter/gpt-4o""#,
        );
        let route = ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-4o".into(),
            provider: Some("openrouter".into()),
        };
        assert_eq!(
            model_row(&row(members)).expect("a current row decodes"),
            legacy(Some(route), Some("openrouter/gpt-4o"))
        );
        let synthetic = r#","route":{"kind":"synthetic","id":"acme/fast"},"reference":"acme/fast""#;
        assert_eq!(
            model_row(&row(synthetic)).expect("a synthetic row decodes"),
            legacy(
                Some(ModelRoute::Synthetic {
                    id: "acme/fast".into(),
                }),
                Some("acme/fast"),
            )
        );
    }

    #[test]
    fn a_route_that_names_no_provider_decodes_without_one() {
        let members = r#","route":{"kind":"api","family":"openai_chat","model":"gpt-4o"}"#;
        let route = ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-4o".into(),
            provider: None,
        };
        assert_eq!(
            model_row(&row(members)).expect("a provider-less route decodes"),
            legacy(Some(route), None)
        );
    }

    #[test]
    fn each_additive_member_decodes_without_the_other() {
        let route = r#","route":{"kind":"harness","id":"dalgon/normal"}"#;
        assert_eq!(
            model_row(&row(route)).expect("a route alone decodes"),
            legacy(
                Some(ModelRoute::Harness {
                    id: "dalgon/normal".into(),
                }),
                None,
            )
        );
        assert_eq!(
            model_row(&row(r#","reference":"openai/gpt-4o""#)).expect("a reference decodes"),
            legacy(None, Some("openai/gpt-4o"))
        );
    }

    #[test]
    fn a_present_route_that_does_not_decode_is_refused_not_read_as_absent() {
        let bad = [
            "null",
            r#""openai/gpt-4o""#,
            "7",
            "{}",
            r#"{"kind":"mystery","id":"acme/fast"}"#,
            r#"{"kind":"api","family":"openai_chat"}"#,
            r#"{"kind":"api","family":"no_such_family","model":"gpt-4o"}"#,
            r#"{"kind":"api","family":"openai_chat","model":"gpt-4o","provider":""}"#,
            r#"{"kind":"api","family":"openai_chat","model":"gpt-4o","provider":7}"#,
            r#"{"kind":"synthetic","id":"NotValid"}"#,
            r#"{"kind":"harness","id":"dalgon/other"}"#,
        ];
        for route in bad {
            refused(&format!(r#","route":{route}"#), "route");
        }
    }

    #[test]
    fn a_present_reference_that_is_not_a_string_is_refused_not_read_as_absent() {
        for reference in ["null", "7", "true", r#"["openai/gpt-4o"]"#, r#"{"a":1}"#] {
            refused(&format!(r#","reference":{reference}"#), "reference");
        }
    }

    #[test]
    fn the_legacy_members_stay_required() {
        for (text, what) in [
            (r#"{"provider":"openai","name":"GPT-4o"}"#, "id"),
            (r#"{"id":"gpt-4o","name":"GPT-4o"}"#, "provider"),
            (r#"{"id":"gpt-4o","provider":"openai"}"#, "name"),
        ] {
            let value: Value = sonic_rs::from_str(text).expect("row json");
            assert!(
                matches!(
                    model_row(&value),
                    Err(WireError::Protocol { message, .. })
                        if message == format!("remote frame has an invalid {what}")
                ),
                "{text} is refused as an invalid {what}"
            );
        }
    }
}
