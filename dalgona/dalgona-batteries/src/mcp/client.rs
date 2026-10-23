// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Session-owned MCP instances, declaration reconciliation and call pipeline.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use dal_agent::{
    error::ServiceError,
    ext::{BoxFuture, Caller, HookCx, McpClient, ObserveHook, Services, Tool},
};
use dal_core::{
    Answer, Choice, McpRequest, McpResponse, Name, Notice, Question, RawJson, Service, SessionId,
    ext::{McpDeclaration, McpServerDecl, Visibility},
};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use super::{
    Budgets, LIST_PAGE_MAX, MRTR_MAX, McpConfig, McpError, RESTART_BUDGET, TransportError,
    http::{
        HttpTransport,
        auth::RefreshCoordinator,
        protocol::{self, LEGACY_PROTOCOL_VERSION, PROTOCOL_VERSION},
    },
    stdio::{ProcessEnvironment, StdioTransport},
    tools::{
        HeaderAnnotation, Key, MappedTool, RemoteTool, ServerEntryTool, ToolListCache, cache_ttl,
        decode_tool_page, fold_tool_name, sanitize_server_text, shape_result, tool_spec,
    },
};

struct DeclaredServer {
    plugin: Name,
    server: Arc<McpServerDecl>,
}

struct Session {
    services: Arc<dyn Services>,
    caller: Caller,
    cancel: CancellationToken,
    declarations: Mutex<HashMap<Key, DeclaredServer>>,
    instances: Mutex<HashMap<Key, Arc<Instance>>>,
    publishing: Mutex<()>,
}

struct Instance {
    key: Key,
    server: Arc<McpServerDecl>,
    next_id: AtomicU64,
    granted: AtomicBool,
    cancel: CancellationToken,
    state: Mutex<InstanceState>,
    starting: Mutex<()>,
    last_notice: Mutex<Option<Instant>>,
}

struct InstanceState {
    phase: Phase,
    restarts_left: u32,
    /// Whether the current `Starting` attempt spent restart budget. A first
    /// start from `Declared` spends nothing; only a restart from `Failed`
    /// does, so a withdrawn first start returns to `Declared`.
    spent: bool,
}

impl InstanceState {
    /// Withdraws a start that never admitted a transport.
    ///
    /// A first start took no budget and returns to `Declared`, so the next
    /// call starts free. A restart refunds its attempt and returns to
    /// `Failed`. Either way the next call starts clean instead of wedging on
    /// `Starting`, and a cancelled attempt never counts as the session's one
    /// restart while a genuine failure still does.
    fn withdraw_start(&mut self) {
        if matches!(self.phase, Phase::Stopped) {
            return;
        }
        if self.spent {
            self.spent = false;
            self.restarts_left = (self.restarts_left + 1).min(RESTART_BUDGET);
            self.phase = Phase::Failed;
        } else {
            self.phase = Phase::Declared;
        }
    }
}

enum Phase {
    Declared,
    Starting,
    Ready(Arc<Ready>),
    Failed,
    Latched,
    Stopped,
}

struct Ready {
    transport: Transport,
    version: String,
    cache: Mutex<ToolListCache>,
}

enum Transport {
    Stdio(StdioTransport),
    Http(Box<HttpTransport>),
}

#[derive(Clone, Copy)]
struct RequestContext<'a> {
    instance: &'a Instance,
    session: &'a Session,
    who: &'a Caller,
    budgets: &'a Budgets,
    client_version: &'a str,
    call_cancel: Option<&'a CancellationToken>,
}

impl RequestContext<'_> {
    fn cancel(&self) -> &CancellationToken {
        self.call_cancel.unwrap_or(&self.instance.cancel)
    }
}

impl Instance {
    fn new(key: Key, server: Arc<McpServerDecl>, cancel: CancellationToken) -> Self {
        Self {
            key,
            server,
            next_id: AtomicU64::new(1),
            granted: AtomicBool::new(false),
            cancel,
            state: Mutex::new(InstanceState {
                phase: Phase::Declared,
                restarts_left: RESTART_BUDGET,
                spent: false,
            }),
            starting: Mutex::new(()),
            last_notice: Mutex::new(None),
        }
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    async fn crash(&self) {
        let mut state = self.state.lock().await;
        state.phase = if state.restarts_left == 0 {
            Phase::Latched
        } else {
            Phase::Failed
        };
    }

    async fn stop(&self, grace: Duration) {
        self.cancel.cancel();
        let ready = {
            let mut state = self.state.lock().await;
            match std::mem::replace(&mut state.phase, Phase::Stopped) {
                Phase::Ready(ready) => Some(ready),
                _ => None,
            }
        };
        if let Some(ready) = ready {
            ready.transport.shutdown(grace).await;
        }
    }

    async fn status(&self) -> (&'static str, usize) {
        let state = self.state.lock().await;
        match &state.phase {
            Phase::Declared => ("declared", 0),
            Phase::Starting => ("starting", 0),
            Phase::Failed => ("failed", 0),
            Phase::Latched => ("latched", 0),
            Phase::Stopped => ("stopped", 0),
            Phase::Ready(ready) => ("ready", ready.cache.lock().await.tools.len()),
        }
    }
}

impl Transport {
    async fn exchange(
        &self,
        method: &str,
        params: &str,
        version: &str,
        annotations: &[HeaderAnnotation],
        arguments: Option<&RawJson>,
        ctx: RequestContext<'_>,
    ) -> Result<RawJson, TransportError> {
        let id = ctx.instance.id();
        let body = protocol::request_body(id, method, params, version, ctx.client_version)?;
        match self {
            Self::Stdio(transport) => {
                let mut inflight = transport.send(id, &body, ctx.cancel()).await?;
                let response = await_reply(id, &mut inflight.events, ctx).await;
                // A reply or a server failure settles the slot. A timeout or
                // a cancellation leaves it unsettled: dropping it retires the
                // slot and tells the server to stop, exactly as when the
                // caller's whole call is dropped before it gets here.
                if matches!(
                    response,
                    Err(TransportError::Cancelled | TransportError::Mcp(McpError::Timeout { .. }))
                ) {
                    drop(inflight);
                } else {
                    inflight.settle().await;
                }
                response
            }
            Self::Http(transport) => {
                let call = super::http::CallCx {
                    method,
                    version,
                    services: ctx.session.services.as_ref(),
                    who: ctx.who,
                    cancel: &ctx.instance.cancel,
                    call_cancel: ctx.call_cancel,
                };
                transport
                    .exchange(
                        id,
                        &ctx.instance.next_id,
                        params,
                        annotations,
                        arguments,
                        &call,
                    )
                    .await
            }
        }
    }

    async fn notify(
        &self,
        method: &str,
        version: &str,
        ctx: RequestContext<'_>,
    ) -> Result<(), TransportError> {
        match self {
            Self::Stdio(transport) => {
                let body = protocol::notification_body(method, version, ctx.client_version)?;
                transport.notify(&body, ctx.cancel()).await
            }
            Self::Http(transport) => {
                let call = super::http::CallCx {
                    method,
                    version,
                    services: ctx.session.services.as_ref(),
                    who: ctx.who,
                    cancel: &ctx.instance.cancel,
                    call_cancel: ctx.call_cancel,
                };
                transport.notify(&ctx.instance.next_id, &call).await
            }
        }
    }

    async fn shutdown(&self, grace: Duration) {
        match self {
            Self::Stdio(transport) => {
                let _ = transport.shutdown(grace).await;
            }
            Self::Http(transport) => {
                let _ = transport.shutdown().await;
            }
        }
    }
}

/// The one MCP client registered for this extension generation.
pub(crate) struct Client {
    config: McpConfig,
    budgets: Budgets,
    refreshes: Arc<RefreshCoordinator>,
    sessions: Mutex<HashMap<SessionId, Arc<Session>>>,
    #[cfg(test)]
    loopback_url: Option<reqwest::Url>,
}

impl Client {
    pub(crate) fn new(config: McpConfig, budgets: Budgets) -> Arc<Self> {
        Arc::new(Self {
            config,
            budgets,
            refreshes: Arc::new(RefreshCoordinator::new()),
            sessions: Mutex::new(HashMap::new()),
            #[cfg(test)]
            loopback_url: None,
        })
    }

    /// Supplies only the internal HTTP transport URL; declarations, host
    /// identity, grants, resolution, publication, and lifecycle stay real.
    #[cfg(test)]
    pub(crate) fn with_loopback_transport(
        config: McpConfig,
        budgets: Budgets,
        url: reqwest::Url,
    ) -> Arc<Self> {
        assert_eq!(url.scheme(), "http", "internal protocol fixture uses HTTP");
        assert_eq!(
            url.host_str(),
            Some("127.0.0.1"),
            "fixture is loopback only"
        );
        let mut client = Self::new(config, budgets);
        Arc::get_mut(&mut client)
            .expect("unshared test client")
            .loopback_url = Some(url);
        client
    }

    fn context<'a>(
        &'a self,
        instance: &'a Instance,
        session: &'a Session,
        who: &'a Caller,
    ) -> RequestContext<'a> {
        RequestContext {
            instance,
            session,
            who,
            budgets: &self.budgets,
            client_version: &self.config.client_version,
            call_cancel: None,
        }
    }

    async fn session(&self, id: SessionId) -> Option<Arc<Session>> {
        self.sessions.lock().await.get(&id).cloned()
    }

    async fn start_session(&self, id: SessionId, cx: HookCx) -> Result<(), ServiceError> {
        let session = Arc::new(Session {
            services: cx.services,
            caller: cx.caller,
            cancel: CancellationToken::new(),
            declarations: Mutex::new(HashMap::new()),
            instances: Mutex::new(HashMap::new()),
            publishing: Mutex::new(()),
        });
        self.sessions.lock().await.insert(id, Arc::clone(&session));
        self.reconcile(id, &session).await
    }

    async fn end_session(&self, id: SessionId) {
        let Some(session) = self.sessions.lock().await.remove(&id) else {
            return;
        };
        session.cancel.cancel();
        let instances: Vec<_> = session
            .instances
            .lock()
            .await
            .drain()
            .map(|(_, instance)| instance)
            .collect();
        for instance in instances {
            instance.stop(self.budgets.shutdown_grace).await;
        }
        let _ = session
            .services
            .add_session_tools(&session.caller, Vec::new())
            .await;
    }

    async fn reconcile(&self, id: SessionId, session: &Arc<Session>) -> Result<(), ServiceError> {
        let feed = session.services.mcp_declarations(&session.caller).await?;
        let next = collect_declarations(id, feed, session);
        let previous = {
            let mut guard = session.declarations.lock().await;
            if same_declarations(&guard, &next) {
                return Ok(());
            }
            std::mem::replace(&mut *guard, next)
        };
        let current = session.declarations.lock().await;
        let obsolete: Vec<_> = {
            let mut instances = session.instances.lock().await;
            let keys: Vec<_> = instances
                .keys()
                .filter(|key| {
                    current.get(*key).is_none_or(|decl| {
                        previous
                            .get(*key)
                            .is_none_or(|old| old.server != decl.server)
                    })
                })
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| instances.remove(&key))
                .collect()
        };
        drop(current);
        for instance in obsolete {
            instance.stop(self.budgets.shutdown_grace).await;
        }
        self.publish(session).await
    }

    async fn publish(&self, session: &Session) -> Result<(), ServiceError> {
        let _guard = session.publishing.lock().await;
        let declarations = session.declarations.lock().await;
        let instances = session.instances.lock().await;
        let mut declared: Vec<Key> = declarations.keys().cloned().collect();
        declared.sort_by_key(Key::display);
        let ready = ready_snapshot(&declared, &instances).await;
        drop(instances);
        let ready_names: BTreeMap<Key, Vec<String>> = ready
            .iter()
            .map(|(key, tools)| {
                (
                    key.clone(),
                    tools.iter().map(|tool| tool.name.clone()).collect(),
                )
            })
            .collect();
        plan_publication(&declared, &ready_names).map_err(|collision| {
            let error = format!(
                "mcp: mapped tool {} collides between {} and {}",
                collision.name,
                collision.first.display(),
                collision.second.display()
            );
            notify_mcp(session, error.clone());
            ServiceError::failed(Some(Service::Mcp), error)
        })?;
        let mut tools = build_tools(session, &declared, &declarations, &ready)?;
        drop(declarations);
        loop {
            match session
                .services
                .add_session_tools(&session.caller, tools.clone())
                .await
            {
                Ok(()) => return Ok(()),
                Err(ServiceError::ToolNameInUse { name, held_by }) => {
                    if held_by.as_ref() == session.caller.ext().as_str() {
                        return Err(ServiceError::ToolNameInUse { name, held_by });
                    }
                    let before = tools.len();
                    tools.retain(|(tool, _)| tool.name().as_str() != name.as_ref());
                    if tools.len() == before {
                        return Err(ServiceError::ToolNameInUse { name, held_by });
                    }
                    notify_mcp(
                        session,
                        format!(
                            "mcp: mapped tool {name} not registered; the name is already in use."
                        ),
                    );
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn resolve(
        &self,
        session: &Arc<Session>,
        who: &Caller,
        req: &McpRequest,
    ) -> Result<(Key, Arc<McpServerDecl>), McpError> {
        let declarations = session.declarations.lock().await;
        let qualified = req.server.rsplit_once('.');
        let registrar = who.ext() == session.caller.ext();
        let found = declarations.iter().find(|(key, value)| {
            (registrar || value.plugin == *who.ext())
                && (key.server == req.server.as_ref()
                    || qualified
                        .is_some_and(|(skill, server)| key.skill == skill && key.server == server))
        });
        found
            .map(|(key, value)| (key.clone(), value.server.clone()))
            .ok_or_else(|| McpError::NotGranted {
                key: req.server.to_string(),
            })
    }

    async fn ready(
        &self,
        session: &Arc<Session>,
        instance: &Arc<Instance>,
        who: &Caller,
    ) -> Result<Arc<Ready>, McpError> {
        {
            let state = instance.state.lock().await;
            match &state.phase {
                Phase::Ready(ready) => return Ok(Arc::clone(ready)),
                Phase::Latched => {
                    return Err(McpError::Latched {
                        key: instance.key.display(),
                    });
                }
                Phase::Stopped => {
                    return Err(McpError::Start {
                        key: instance.key.display(),
                        cause: "session ended".into(),
                    });
                }
                _ => {}
            }
        }
        let _starting = instance.starting.lock().await;
        {
            let mut state = instance.state.lock().await;
            match &state.phase {
                Phase::Ready(ready) => return Ok(Arc::clone(ready)),
                Phase::Latched => {
                    return Err(McpError::Latched {
                        key: instance.key.display(),
                    });
                }
                Phase::Stopped => {
                    return Err(McpError::Start {
                        key: instance.key.display(),
                        cause: "session ended".into(),
                    });
                }
                Phase::Failed if state.restarts_left == 0 => {
                    state.phase = Phase::Latched;
                    return Err(McpError::Latched {
                        key: instance.key.display(),
                    });
                }
                // A previous starter was dropped (its call was cancelled)
                // before it admitted a transport. Only the holder of the
                // starting lock sets this phase, so finding it set means the
                // attempt never ran: withdraw it, then price the withdrawn
                // phase below exactly like a fresh attempt, so a cancelled
                // restart is priced again instead of running free.
                Phase::Starting => state.withdraw_start(),
                _ => {}
            }
            // A start from Failed spends one restart; a first start is free.
            if matches!(state.phase, Phase::Failed) {
                state.restarts_left = state.restarts_left.saturating_sub(1);
                state.spent = true;
            }
            state.phase = Phase::Starting;
        }
        let started = self.start_transport(session, instance, who).await;
        self.admit_started(session, instance, started).await
    }

    /// Moves one started transport into the ready phase and publishes its
    /// tools, crashing and shutting it down when either step fails.
    async fn admit_started(
        &self,
        session: &Arc<Session>,
        instance: &Arc<Instance>,
        started: Result<Arc<Ready>, McpError>,
    ) -> Result<Arc<Ready>, McpError> {
        match started {
            Ok(ready) => {
                let mut state = instance.state.lock().await;
                if instance.cancel.is_cancelled() || matches!(state.phase, Phase::Stopped) {
                    drop(state);
                    ready.transport.shutdown(self.budgets.shutdown_grace).await;
                    return Err(McpError::Start {
                        key: instance.key.display(),
                        cause: "session ended".into(),
                    });
                }
                state.phase = Phase::Ready(Arc::clone(&ready));
                drop(state);
                if let Err(error) = self.publish(session).await {
                    instance.crash().await;
                    ready.transport.shutdown(self.budgets.shutdown_grace).await;
                    return Err(McpError::Start {
                        key: instance.key.display(),
                        cause: error.to_string(),
                    });
                }
                Ok(ready)
            }
            Err(error) => {
                // A start the session cancelled never ran: it is withdrawn. A
                // genuine start failure keeps the attempt it took, exactly
                // like a running crash.
                if instance.cancel.is_cancelled() {
                    instance.state.lock().await.withdraw_start();
                } else {
                    instance.crash().await;
                }
                Err(error)
            }
        }
    }

    async fn start_transport(
        &self,
        session: &Session,
        instance: &Instance,
        who: &Caller,
    ) -> Result<Arc<Ready>, McpError> {
        let transport = match instance.server.as_ref() {
            McpServerDecl::Stdio { .. } => {
                let env = process_environment(session, who).await?;
                let transport = StdioTransport::start(
                    instance.key.clone(),
                    &instance.server,
                    &env,
                    &self.budgets,
                )
                .await?;
                Transport::Stdio(transport)
            }
            McpServerDecl::Http { endpoint } => {
                let url = endpoint.as_url();
                #[cfg(test)]
                let url = self.loopback_url.as_ref().unwrap_or(url);
                let transport = self.http_transport(instance, url)?;
                Transport::Http(Box::new(transport))
            }
        };
        self.admit_transport(transport, instance, session, who)
            .await
    }

    /// Builds the HTTP transport for one instance from its endpoint URL.
    ///
    /// The declaration's single parsed URL is cloned into the transport;
    /// no endpoint is parsed or compared a second time here.
    fn http_transport(
        &self,
        instance: &Instance,
        url: &reqwest::Url,
    ) -> Result<HttpTransport, McpError> {
        HttpTransport::new(
            instance.key.clone(),
            url.clone(),
            self.config.tokens_path.clone(),
            self.config.client_version.clone(),
            &self.budgets,
            Arc::clone(&self.refreshes),
        )
    }

    /// Walks one freshly built transport through handshake and tool listing.
    ///
    /// Every transport becomes ready only through this path; a transport
    /// that fails either step is shut down before the error returns.
    async fn admit_transport(
        &self,
        transport: Transport,
        instance: &Instance,
        session: &Session,
        who: &Caller,
    ) -> Result<Arc<Ready>, McpError> {
        let started = self.handshake(&transport, instance, session, who).await;
        let version = match started {
            Ok(version) => version,
            Err(error) => {
                transport.shutdown(self.budgets.shutdown_grace).await;
                return Err(error);
            }
        };
        let cache = match self
            .list(&transport, instance, session, who, &version)
            .await
        {
            Ok(cache) => cache,
            Err(error) => {
                transport.shutdown(self.budgets.shutdown_grace).await;
                return Err(error);
            }
        };
        Ok(Arc::new(Ready {
            transport,
            version,
            cache: Mutex::new(cache),
        }))
    }

    async fn handshake(
        &self,
        transport: &Transport,
        instance: &Instance,
        session: &Session,
        who: &Caller,
    ) -> Result<String, McpError> {
        let probe = tokio::time::timeout(
            self.budgets.discover,
            transport.exchange(
                "server/discover",
                "{}",
                PROTOCOL_VERSION,
                &[],
                None,
                self.context(instance, session, who),
            ),
        )
        .await;
        match probe {
            Ok(Ok(body)) => {
                if let Some(McpError::Protocol {
                    code: -32022,
                    message,
                }) = protocol::json_rpc_error(body.as_str())
                {
                    if !protocol::advertises_legacy(&message) {
                        return Err(McpError::Protocol {
                            code: -32022,
                            message,
                        });
                    }
                    let second = transport
                        .exchange(
                            "server/discover",
                            "{}",
                            LEGACY_PROTOCOL_VERSION,
                            &[],
                            None,
                            self.context(instance, session, who),
                        )
                        .await
                        .map_err(map_transport)?;
                    if let Some(error) = protocol::json_rpc_error(second.as_str()) {
                        return Err(error);
                    }
                    return Ok(LEGACY_PROTOCOL_VERSION.to_owned());
                }
                if let Some(McpError::Protocol {
                    code: -32020 | -32021,
                    ..
                }) = protocol::json_rpc_error(body.as_str())
                {
                    return Ok(PROTOCOL_VERSION.to_owned());
                }
                if protocol::json_rpc_error(body.as_str()).is_none() {
                    return Ok(PROTOCOL_VERSION.to_owned());
                }
            }
            Ok(Err(TransportError::Cancelled)) => {
                return Err(McpError::Start {
                    key: instance.key.display(),
                    cause: "session cancelled".into(),
                });
            }
            _ => {}
        }
        let params = format!(
            "{{\"protocolVersion\":\"{LEGACY_PROTOCOL_VERSION}\",\"capabilities\":{{}},\"clientInfo\":{{\"name\":\"dalgona\",\"version\":{}}}}}",
            sonic_rs::to_string(&self.config.client_version).map_err(|error| McpError::Start {
                key: instance.key.display(),
                cause: error.to_string(),
            })?
        );
        let result = transport
            .exchange(
                "initialize",
                &params,
                LEGACY_PROTOCOL_VERSION,
                &[],
                None,
                self.context(instance, session, who),
            )
            .await
            .map_err(map_transport)?;
        if let Some(error) = protocol::json_rpc_error(result.as_str()) {
            return Err(error);
        }
        transport
            .notify(
                "notifications/initialized",
                LEGACY_PROTOCOL_VERSION,
                self.context(instance, session, who),
            )
            .await
            .map_err(map_transport)?;
        Ok(LEGACY_PROTOCOL_VERSION.to_owned())
    }

    async fn list(
        &self,
        transport: &Transport,
        instance: &Instance,
        session: &Session,
        who: &Caller,
        version: &str,
    ) -> Result<ToolListCache, McpError> {
        let deadline = Instant::now() + self.budgets.list;
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut ttl = None;
        for page_no in 0..LIST_PAGE_MAX {
            let params = match &cursor {
                Some(cursor) => format!(
                    "{{\"cursor\":{}}}",
                    sonic_rs::to_string(cursor)
                        .map_err(|error| protocol_error(error.to_string()))?
                ),
                None => "{}".to_owned(),
            };
            let exchange = transport.exchange(
                "tools/list",
                &params,
                version,
                &[],
                None,
                self.context(instance, session, who),
            );
            let remaining = deadline.saturating_duration_since(Instant::now());
            let reply = tokio::time::timeout(remaining, exchange)
                .await
                .map_err(|_| McpError::Timeout {
                    n: self.budgets.list.as_secs(),
                })?
                .map_err(map_transport)?;
            let result = response_result(&reply)?;
            let page = decode_tool_page(
                &sonic_rs::to_string(&result).map_err(|error| protocol_error(error.to_string()))?,
            )?;
            for excluded in page.excluded {
                session.services.notify(
                    &session.caller,
                    Notice {
                        turn: None,
                        kind: "mcp".into(),
                        text: excluded.warning.into(),
                    },
                );
            }
            ttl = page.ttl_ms.or(ttl);
            tools.extend(page.tools);
            cursor = page.next_cursor;
            if cursor.is_none() {
                return Ok(ToolListCache {
                    until: Instant::now() + cache_ttl(ttl),
                    tools,
                });
            }
            if page_no + 1 == LIST_PAGE_MAX {
                return Err(McpError::ListPages);
            }
        }
        Err(McpError::ListPages)
    }

    async fn call_server(
        &self,
        session: &Arc<Session>,
        who: &Caller,
        req: McpRequest,
        cancel: &CancellationToken,
    ) -> Result<McpResponse, McpError> {
        self.reconcile(req.session, session)
            .await
            .map_err(|error| McpError::Start {
                key: req.server.to_string(),
                cause: error.to_string(),
            })?;
        let (key, server) = self.resolve(session, who, &req).await?;
        let instance = {
            let mut instances = session.instances.lock().await;
            Arc::clone(instances.entry(key.clone()).or_insert_with(|| {
                Arc::new(Instance::new(key, server, session.cancel.child_token()))
            }))
        };
        self.dispatch_call(session, who, &req, &instance, cancel)
            .await
    }

    /// Runs one resolved request against its instance: grant marking, cache
    /// refresh, and the remote call with crash handling.
    async fn dispatch_call(
        &self,
        session: &Arc<Session>,
        who: &Caller,
        req: &McpRequest,
        instance: &Arc<Instance>,
        cancel: &CancellationToken,
    ) -> Result<McpResponse, McpError> {
        instance.granted.store(true, Ordering::SeqCst);
        let ready = self.ready(session, instance, who).await?;
        if req.tool.is_empty() {
            let tools = ready
                .cache
                .lock()
                .await
                .tools
                .iter()
                .map(|tool| fold_tool_name(&instance.key.skill, &instance.key.server, &tool.name))
                .collect::<Vec<_>>();
            let text = if tools.is_empty() {
                format!("mcp server {} has no tools.", instance.key.display())
            } else {
                tools.join("\n")
            };
            return Ok(McpResponse {
                text: text.into(),
                is_error: false,
            });
        }
        let (remote, refreshed) = self
            .find_tool(&ready, instance, session, who, &req.tool)
            .await?;
        if refreshed {
            self.publish(session)
                .await
                .map_err(|error| McpError::Start {
                    key: instance.key.display(),
                    cause: error.to_string(),
                })?;
        }
        let remote = remote.ok_or_else(|| McpError::NotFound {
            key: instance.key.display(),
            tool: req.tool.to_string(),
        })?;
        let result = self
            .call_tool(&ready, instance, session, who, req, &remote, cancel)
            .await;
        if matches!(
            result,
            Err(McpError::Exited { .. } | McpError::InvalidLine { .. })
        ) {
            instance.crash().await;
            self.publish(session)
                .await
                .map_err(|error| McpError::Start {
                    key: instance.key.display(),
                    cause: error.to_string(),
                })?;
            ready.transport.shutdown(self.budgets.shutdown_grace).await;
        }
        result
    }

    async fn find_tool(
        &self,
        ready: &Ready,
        instance: &Instance,
        session: &Session,
        who: &Caller,
        tool: &str,
    ) -> Result<(Option<RemoteTool>, bool), McpError> {
        let mut cache = ready.cache.lock().await;
        let refreshed = Instant::now() >= cache.until;
        if refreshed {
            *cache = self
                .list(&ready.transport, instance, session, who, &ready.version)
                .await?;
        }
        Ok((
            cache.tools.iter().find(|item| item.name == tool).cloned(),
            refreshed,
        ))
    }

    async fn call_tool(
        &self,
        ready: &Ready,
        instance: &Instance,
        session: &Session,
        who: &Caller,
        req: &McpRequest,
        tool: &RemoteTool,
        cancel: &CancellationToken,
    ) -> Result<McpResponse, McpError> {
        let ctx = RequestContext {
            call_cancel: Some(cancel),
            ..self.context(instance, session, who)
        };
        let mut responses: Option<String> = None;
        let mut request_state: Option<String> = None;
        for attempt in 0..=MRTR_MAX {
            if cancel.is_cancelled() {
                return Err(map_transport(TransportError::Cancelled));
            }
            let params = call_params(
                &req.tool,
                &req.arguments,
                responses.as_deref(),
                request_state.as_deref(),
            )?;
            let reply = ready
                .transport
                .exchange(
                    "tools/call",
                    &params,
                    &ready.version,
                    &tool.headers,
                    Some(&req.arguments),
                    ctx,
                )
                .await
                .map_err(map_transport)?;
            let result = response_result(&reply)?;
            let result_type = result
                .get("resultType")
                .and_then(JsonValueTrait::as_str)
                .unwrap_or("complete");
            let input_required = result_type == "input_required";
            // The reply of the last permitted request has no retry left to
            // carry answers, so it is terminal before anything is asked.
            if input_required && attempt == MRTR_MAX {
                break;
            }
            if input_required {
                responses = Some(answer_inputs(&result, session, who, cancel).await?);
                request_state = result
                    .get("requestState")
                    .map(sonic_rs::to_string)
                    .transpose()
                    .map_err(|error| protocol_error(error.to_string()))?;
                continue;
            }
            let content = result
                .get("content")
                .and_then(|value| value.as_array())
                .map(|items| items.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            let is_error = result
                .get("isError")
                .and_then(JsonValueTrait::as_bool)
                .unwrap_or(false);
            let shaped = shape_result(&content, is_error, Some(result_type))?;
            return Ok(McpResponse {
                text: shaped.text.into(),
                is_error: shaped.is_error,
            });
        }
        Err(McpError::InputRequiredLimit)
    }

    pub(crate) async fn status(&self, id: SessionId) -> String {
        let Some(session) = self.session(id).await else {
            return "no MCP servers in this session.".into();
        };
        if let Err(error) = self.reconcile(id, &session).await {
            return error.to_string();
        }
        let declarations = session.declarations.lock().await;
        if declarations.is_empty() {
            return "no MCP servers in this session.".into();
        }
        let instances = session.instances.lock().await;
        // A start whose owning call was cancelled leaves `Starting` behind
        // with no owner. Only a lock holder sets that phase, so finding it
        // set while the lock is free means the attempt never ran: withdrawing
        // it here keeps a cancelled start from reading as stuck.
        for instance in instances.values() {
            if let Ok(_starting) = instance.starting.try_lock() {
                let mut state = instance.state.lock().await;
                if matches!(state.phase, Phase::Starting) {
                    state.withdraw_start();
                }
            }
        }
        let mut rows = Vec::new();
        for (key, decl) in declarations.iter() {
            let (state, count, grant) = if let Some(instance) = instances.get(key) {
                let (state, count) = instance.status().await;
                let grant = if instance.granted.load(Ordering::SeqCst) {
                    "granted"
                } else {
                    "unasked"
                };
                (state, count, grant)
            } else {
                ("declared", 0, "unasked")
            };
            let transport = match decl.server.as_ref() {
                McpServerDecl::Stdio { .. } => "stdio",
                McpServerDecl::Http { .. } => "http",
            };
            let display = key.display();
            let row = format!("- {display} | {transport} | {state} | {count} tools | {grant}");
            rows.push((display, row));
        }
        // Rows order by display key byte order; keys are unique, so the key
        // alone decides and the row text never does.
        rows.sort();
        let mut body = String::from("MCP servers in this session:");
        for (_, row) in rows {
            body.push('\n');
            body.push_str(&row);
        }
        body
    }
}

impl McpClient for Client {
    fn call<'a>(
        &'a self,
        who: &'a Caller,
        req: McpRequest,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<McpResponse, ServiceError>> {
        Box::pin(async move {
            let Some(session) = self.session(req.session).await else {
                return Err(ServiceError::failed(
                    Some(Service::Mcp),
                    McpError::Start {
                        key: req.server.to_string(),
                        cause: "no session context".into(),
                    }
                    .to_string(),
                ));
            };
            let result = self.call_server(&session, who, req, &cancel).await;
            if session.cancel.is_cancelled() || (result.is_err() && cancel.is_cancelled()) {
                return Err(ServiceError::Cancelled);
            }
            result.map_err(|error| ServiceError::failed(Some(Service::Mcp), error.to_string()))
        })
    }
}

fn collect_declarations(
    id: SessionId,
    feed: Vec<McpDeclaration>,
    session: &Session,
) -> HashMap<Key, DeclaredServer> {
    let mut declarations = HashMap::new();
    let mut seen = HashSet::new();
    for record in feed {
        for (server, decl) in record.block.servers {
            if !seen.insert((record.plugin.clone(), server.clone())) {
                session.services.notify(
                    &session.caller,
                    Notice {
                        turn: None,
                        kind: "mcp".into(),
                        text: format!(
                            "mcp: server {server} is declared by multiple skills of {}",
                            record.plugin
                        )
                        .into(),
                    },
                );
                continue;
            }
            let key = Key {
                session: id,
                skill: record.skill.as_str().to_owned(),
                server: server.into(),
            };
            declarations.insert(
                key,
                DeclaredServer {
                    plugin: record.plugin.clone(),
                    server: Arc::new(decl),
                },
            );
        }
    }
    declarations
}

fn same_declarations(
    old: &HashMap<Key, DeclaredServer>,
    new: &HashMap<Key, DeclaredServer>,
) -> bool {
    old.len() == new.len()
        && old.iter().all(|(key, declaration)| {
            new.get(key).is_some_and(|next| {
                next.plugin == declaration.plugin && next.server == declaration.server
            })
        })
}

/// One mapped tool a publication would register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedTool {
    pub(crate) key: Key,
    pub(crate) remote: String,
    pub(crate) folded: String,
}

/// The names one publication would advertise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublicationPlan {
    pub(crate) entries: Vec<Key>,
    pub(crate) mapped: Vec<PlannedTool>,
}

/// Two isolation keys that fold to one mapped name.
#[derive(Clone, Debug)]
pub(crate) struct NameCollision {
    pub(crate) name: String,
    pub(crate) first: Key,
    pub(crate) second: Key,
}

/// Computes the publication plan from the declared keys and each ready
/// instance's cached remote tool names: one entry tool per declared key
/// plus one mapped tool per cached remote tool. Two keys folding to the
/// same name fail the plan with the name and both keys.
pub(crate) fn plan_publication(
    declared: &[Key],
    ready: &BTreeMap<Key, Vec<String>>,
) -> Result<PublicationPlan, Box<NameCollision>> {
    let mut seen: BTreeMap<String, Key> = BTreeMap::new();
    for key in declared {
        claim_name(&mut seen, &fold_tool_name(&key.skill, &key.server, ""), key)?;
    }
    let mut mapped = Vec::new();
    for (key, tools) in ready {
        for remote in tools {
            let folded = fold_tool_name(&key.skill, &key.server, remote);
            claim_name(&mut seen, &folded, key)?;
            mapped.push(PlannedTool {
                key: key.clone(),
                remote: remote.clone(),
                folded,
            });
        }
    }
    Ok(PublicationPlan {
        entries: declared.to_vec(),
        mapped,
    })
}

/// Claims `folded` for `key`; a second claimant fails with both keys.
fn claim_name(
    seen: &mut BTreeMap<String, Key>,
    folded: &str,
    key: &Key,
) -> Result<(), Box<NameCollision>> {
    match seen.insert(folded.to_owned(), key.clone()) {
        Some(other) => Err(Box::new(NameCollision {
            name: folded.to_owned(),
            first: other,
            second: key.clone(),
        })),
        None => Ok(()),
    }
}

/// Snapshots the cached remote tools of every declared key whose instance
/// is ready; keys without a ready instance contribute nothing.
async fn ready_snapshot(
    declared: &[Key],
    instances: &HashMap<Key, Arc<Instance>>,
) -> BTreeMap<Key, Vec<RemoteTool>> {
    let mut ready = BTreeMap::new();
    for key in declared {
        let Some(instance) = instances.get(key) else {
            continue;
        };
        let state = instance.state.lock().await;
        let Phase::Ready(ready_phase) = &state.phase else {
            continue;
        };
        let cache = ready_phase.cache.lock().await;
        ready.insert(key.clone(), cache.tools.clone());
    }
    ready
}

/// One tool staged for registration with its visibility.
type StagedTool = (Arc<dyn Tool>, Visibility);

/// Builds the deferred tool set for one publication: one entry tool per
/// declared key plus one mapped tool per cached remote tool. A remote tool
/// whose folded name leaves the mapped-tool grammar warns and is skipped;
/// the rest still publish.
fn build_tools(
    session: &Session,
    declared: &[Key],
    declarations: &HashMap<Key, DeclaredServer>,
    ready: &BTreeMap<Key, Vec<RemoteTool>>,
) -> Result<Vec<StagedTool>, ServiceError> {
    let mut tools: Vec<StagedTool> = Vec::new();
    for key in declared {
        let Some(declaration) = declarations.get(key) else {
            continue;
        };
        let entry = ServerEntryTool::new(
            key.clone(),
            declaration.plugin.clone(),
            Arc::clone(&declaration.server),
        )
        .map_err(|error| ServiceError::failed(Some(Service::Mcp), error.to_string()))?;
        tools.push((Arc::new(entry), Visibility::Deferred));
        for remote in ready.get(key).into_iter().flatten() {
            match map_remote_tool(key, declaration, remote) {
                Ok(tool) => tools.push((tool, Visibility::Deferred)),
                Err(warning) => notify_mcp(session, warning),
            }
        }
    }
    Ok(tools)
}

/// Builds one mapped tool for a ready server's cached remote tool, or the
/// warning text when its folded name is outside the mapped-tool grammar.
fn map_remote_tool(
    key: &Key,
    declaration: &DeclaredServer,
    remote: &RemoteTool,
) -> Result<Arc<dyn Tool>, String> {
    let folded = fold_tool_name(&key.skill, &key.server, &remote.name);
    let name = Name::parse_mapped_tool(&folded)
        .map_err(|error| format!("mcp: {folded} is not a valid mapped tool name: {error}"))?;
    Ok(Arc::new(MappedTool {
        key: key.clone(),
        plugin: declaration.plugin.clone(),
        declaration: Arc::clone(&declaration.server),
        remote: remote.name.clone(),
        spec: Arc::new(tool_spec(name, remote)),
    }))
}

/// Shows one `mcp` warning through the session's notice channel.
fn notify_mcp(session: &Session, text: String) {
    session.services.notify(
        &session.caller,
        Notice {
            turn: None,
            kind: "mcp".into(),
            text: text.into(),
        },
    );
}

async fn process_environment(
    session: &Session,
    who: &Caller,
) -> Result<ProcessEnvironment, McpError> {
    let path = env_key(session, who, "PATH").await?;
    let home = env_key(session, who, "HOME").await?;
    let tmpdir = env_key(session, who, "TMPDIR").await?;
    Ok(ProcessEnvironment { path, home, tmpdir })
}

async fn env_key(session: &Session, who: &Caller, key: &str) -> Result<Option<OsString>, McpError> {
    session
        .services
        .env(who, key)
        .await
        .map(|value| value.map(OsString::from))
        .map_err(|error| McpError::Start {
            key: key.to_owned(),
            cause: error.to_string(),
        })
}

fn response_result(reply: &RawJson) -> Result<Value, McpError> {
    let value = reply
        .decode_as::<Value>()
        .map_err(|error| protocol_error(error.to_string()))?;
    if let Some(error) = protocol::json_rpc_error(reply.as_str()) {
        return Err(error);
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| protocol_error("MCP response has no result".into()))
}

fn protocol_error(message: String) -> McpError {
    McpError::Protocol {
        code: -32600,
        message,
    }
}

fn map_transport(error: TransportError) -> McpError {
    match error {
        TransportError::Mcp(error) => error,
        TransportError::Cancelled => McpError::Start {
            key: "session".into(),
            cause: "session cancelled".into(),
        },
    }
}

fn call_params(
    tool: &str,
    arguments: &RawJson,
    responses: Option<&str>,
    request_state: Option<&str>,
) -> Result<String, McpError> {
    let tool = sonic_rs::to_string(tool).map_err(|error| protocol_error(error.to_string()))?;
    let mut params = format!("{{\"name\":{tool},\"arguments\":{}", arguments.as_str());
    if let Some(responses) = responses {
        params.push_str(",\"inputResponses\":");
        params.push_str(responses);
    }
    if let Some(state) = request_state {
        params.push_str(",\"requestState\":");
        params.push_str(state);
    }
    params.push('}');
    Ok(params)
}

async fn answer_inputs(
    result: &Value,
    session: &Session,
    who: &Caller,
    cancel: &CancellationToken,
) -> Result<String, McpError> {
    if cancel.is_cancelled() {
        return Err(map_transport(TransportError::Cancelled));
    }
    let Some(requests) = result
        .get("inputRequests")
        .and_then(|value| value.as_object())
    else {
        return Ok("{}".into());
    };
    let mut answers = Vec::new();
    for (name, request) in requests {
        let name: &str = name;
        let answer = if request.get("method").and_then(JsonValueTrait::as_str)
            == Some("elicitation/create")
        {
            answer_elicitation(request, session, who, cancel).await?
        } else {
            "{\"action\":\"decline\"}".into()
        };
        let name = sonic_rs::to_string(name).map_err(|error| protocol_error(error.to_string()))?;
        answers.push(format!("{name}:{answer}"));
    }
    Ok(format!("{{{}}}", answers.join(",")))
}

async fn answer_elicitation(
    request: &Value,
    session: &Session,
    who: &Caller,
    cancel: &CancellationToken,
) -> Result<String, McpError> {
    let params = request.get("params");
    let message = sanitize_server_text(
        params
            .and_then(|value| value.get("message"))
            .and_then(JsonValueTrait::as_str)
            .unwrap_or("MCP server requests input"),
    );
    let properties = params
        .and_then(|value| value.get("requestedSchema"))
        .and_then(|value| value.get("properties"))
        .and_then(|value| value.as_object());
    let Some(properties) = properties else {
        if cancel.is_cancelled() {
            return Err(map_transport(TransportError::Cancelled));
        }
        return Ok("{\"action\":\"decline\"}".into());
    };
    let mut content = Vec::new();
    for (name, schema) in properties {
        let name: &str = name;
        let prompt = format!("{message}: {name}");
        let question = if let Some(variants) = schema.get("enum").and_then(|value| value.as_array())
        {
            Question::Select {
                prompt: prompt.into(),
                options: variants
                    .iter()
                    .filter_map(JsonValueTrait::as_str)
                    .map(|label| Choice {
                        label: label.into(),
                        description: None,
                    })
                    .collect(),
                multi: false,
                preview: None,
            }
        } else {
            Question::Text {
                prompt: prompt.into(),
                placeholder: None,
            }
        };
        let answer = session.services.ask(who, question).await;
        let Ok(Some(Answer::Value(value))) = answer else {
            if answer.is_err() {
                return Err(McpError::NoAskFrontEnd);
            }
            if cancel.is_cancelled() {
                return Err(map_transport(TransportError::Cancelled));
            }
            return Ok("{\"action\":\"decline\"}".into());
        };
        let name = sonic_rs::to_string(name).map_err(|error| protocol_error(error.to_string()))?;
        content.push(format!("{name}:{}", value.as_str()));
    }
    Ok(format!(
        "{{\"action\":\"accept\",\"content\":{{{}}}}}",
        content.join(",")
    ))
}

async fn call_cancelled(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

/// Waits for a correlated reply; matching progress extends only this call's idle deadline.
async fn await_reply(
    id: u64,
    events: &mut mpsc::Receiver<Result<RawJson, McpError>>,
    ctx: RequestContext<'_>,
) -> Result<RawJson, TransportError> {
    let start = Instant::now();
    let mut deadline = start + ctx.budgets.call;
    let cap = start + ctx.budgets.call_max;
    let progress_token = format!("t-{id}");
    loop {
        let effective = deadline.min(cap);
        let event = tokio::select! {
            biased;
            event = tokio::time::timeout_at(effective.into(), events.recv()) => event,
            () = ctx.instance.cancel.cancelled() => return Err(TransportError::Cancelled),
            () = call_cancelled(ctx.call_cancel) => return Err(TransportError::Cancelled),
        };
        let event = match event {
            Ok(Some(event)) => event,
            Ok(None) => {
                return Err(TransportError::Mcp(McpError::Exited {
                    key: ctx.instance.key.display(),
                    code: -1,
                    diagnostic: String::new(),
                }));
            }
            Err(_) => {
                return Err(TransportError::Mcp(McpError::Timeout {
                    n: if Instant::now() >= cap {
                        ctx.budgets.call_max.as_secs()
                    } else {
                        ctx.budgets.call.as_secs()
                    },
                }));
            }
        };
        let reply = event.map_err(TransportError::Mcp)?;
        let value = reply
            .decode_as::<Value>()
            .map_err(|error| TransportError::Mcp(protocol_error(error.to_string())))?;
        match classify_stream_event(&value, id, &progress_token) {
            StreamEvent::Progress => {
                deadline = Instant::now() + ctx.budgets.call;
                let mut last_notice = ctx.instance.last_notice.lock().await;
                if last_notice.is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1)) {
                    let text = sanitize_server_text(
                        value
                            .get("params")
                            .and_then(|params| params.get("message"))
                            .and_then(JsonValueTrait::as_str)
                            .unwrap_or("progress"),
                    );
                    ctx.session.services.notify(
                        ctx.who,
                        Notice {
                            turn: None,
                            kind: "mcp".into(),
                            text: format!("{}: {text}", ctx.instance.key.display()).into(),
                        },
                    );
                    *last_notice = Some(Instant::now());
                }
            }
            StreamEvent::Reply => return Ok(reply),
            StreamEvent::Ignored => {}
        }
    }
}

/// One event class on a call's response stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StreamEvent {
    /// Matching progress: the call's idle deadline resets.
    Progress,
    /// The correlated reply for this call id.
    Reply,
    /// Everything else: unknown notifications, foreign tokens, other ids.
    Ignored,
}

/// Classifies one decoded response-stream message for the call `id` with
/// progress token `progress_token`. A matching progress notification and
/// the correlated reply are the only events that act; every other message
/// — unknown notification methods, foreign progress tokens, replies to
/// other ids, and server-initiated requests — is skipped as unknown traffic.
///
/// A reply carries no `method`. A server request such as the legacy
/// `elicitation/create` has its own id namespace, so a request whose id
/// equals this call's id is still not this call's reply.
pub(crate) fn classify_stream_event(value: &Value, id: u64, progress_token: &str) -> StreamEvent {
    let progress =
        value.get("method").and_then(JsonValueTrait::as_str) == Some("notifications/progress");
    let token = value
        .get("params")
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| meta.get("progressToken"))
        .and_then(JsonValueTrait::as_str);
    if progress && token == Some(progress_token) {
        return StreamEvent::Progress;
    }
    if value.get("method").is_none() && value.get("id").and_then(JsonValueTrait::as_u64) == Some(id)
    {
        return StreamEvent::Reply;
    }
    StreamEvent::Ignored
}

pub(crate) struct McpCommand(pub(crate) Arc<Client>);

impl dal_agent::ext::CommandHandler for McpCommand {
    fn run<'a>(
        &'a self,
        args: &'a str,
        cx: dal_agent::ext::CommandCx<'a>,
    ) -> BoxFuture<'a, Result<dal_core::Reply, ServiceError>> {
        Box::pin(async move {
            if !args.trim().is_empty() {
                return Ok(dal_core::Reply::Done(dal_core::Output::Text(
                    "usage: /mcp".into(),
                )));
            }
            Ok(dal_core::Reply::Done(dal_core::Output::Markdown(
                self.0.status(cx.session()).await.into(),
            )))
        })
    }
}

pub(crate) struct SessionStartHook(pub(crate) Arc<Client>);

impl ObserveHook<dal_core::SessionStart> for SessionStartHook {
    fn call(
        &self,
        input: dal_core::SessionStart,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<(), dal_agent::ext::HookError>> {
        let client = Arc::clone(&self.0);
        Box::pin(async move {
            client
                .start_session(input.session, cx)
                .await
                .map_err(|error| dal_agent::ext::HookError::Failed {
                    message: error.to_string().into(),
                })
        })
    }
}

pub(crate) struct SessionEndHook(pub(crate) Arc<Client>);

impl ObserveHook<dal_core::SessionEnd> for SessionEndHook {
    fn call(
        &self,
        input: dal_core::SessionEnd,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<(), dal_agent::ext::HookError>> {
        let client = Arc::clone(&self.0);
        Box::pin(async move {
            client.end_session(input.session).await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use dal_agent::{
        error::ServiceError,
        ext::{
            Caller, Doc, EventStream, HookCx, RawValue, Services, Tool, ToolOutcome,
            services::ServiceFuture,
        },
    };
    use dal_core::{
        AgentsOp, AgentsReply, Answer, EntryId, FetchRequest, FetchResponse, Inference, JobsOp,
        JobsReply, ModelRequest, Notice, PrivateEffect, Question, RawJson, RunOutput, RunRequest,
        SessionId, SidecarOp, TurnOp, TurnOpReply,
        ext::{
            McpDeclaration, McpRequest, McpResponse, StateError, StateOp, StateRecord, Visibility,
        },
    };
    use tokio::sync::{Mutex, mpsc};
    use tokio_util::sync::CancellationToken;

    use super::{
        Budgets, DeclaredServer, Instance, McpError, RequestContext, Session, TransportError,
        await_reply,
    };
    use crate::mcp::tools::Key;

    struct TestServices;

    fn unavailable<T: Send + 'static>() -> ServiceFuture<'static, T> {
        Box::pin(async {
            Err(ServiceError::failed(
                None,
                "unavailable in the client unit fixture",
            ))
        })
    }

    impl Services for TestServices {
        fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
            unavailable()
        }

        fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
            unavailable()
        }

        fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
            unavailable()
        }

        fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
            unavailable()
        }

        fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
            unavailable()
        }

        fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
            unavailable()
        }

        fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
            unavailable()
        }

        fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
            unavailable()
        }

        fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
            unavailable()
        }

        fn jobs(&self, _who: &Caller, _op: JobsOp) -> ServiceFuture<'_, JobsReply> {
            unavailable()
        }

        fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
            unavailable()
        }

        fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
            unavailable()
        }

        fn add_session_tools(
            &self,
            _who: &Caller,
            _tools: Vec<(Arc<dyn Tool>, Visibility)>,
        ) -> ServiceFuture<'_, ()> {
            unavailable()
        }

        fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
            unavailable()
        }

        fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<Doc>> {
            unavailable()
        }

        fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
            unavailable()
        }

        fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
            unavailable()
        }

        fn sidecar(&self, _who: &Caller, _op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
            unavailable()
        }

        fn state(
            &self,
            _who: &Caller,
            _op: StateOp,
        ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
            unavailable()
        }

        fn model_ready(
            &self,
            _who: &Caller,
            _model: dal_core::ModelRoute,
        ) -> ServiceFuture<'_, Option<dal_core::ModelRoute>> {
            unavailable()
        }

        fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
            unavailable()
        }

        fn infer_stream(
            &self,
            _who: &Caller,
            _req: ModelRequest,
        ) -> ServiceFuture<'_, EventStream> {
            unavailable()
        }

        fn call_tool(
            &self,
            _who: &Caller,
            _name: &str,
            _args: Box<RawValue>,
        ) -> ServiceFuture<'_, ToolOutcome> {
            unavailable()
        }

        fn notify(&self, _who: &Caller, _notice: Notice) {}

        fn append_record(
            &self,
            _who: &Caller,
            _kind: &str,
            _body: Box<RawValue>,
        ) -> ServiceFuture<'_, EntryId> {
            unavailable()
        }

        fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
            unavailable()
        }

        fn private_outcomes(&self, _who: &Caller) -> ServiceFuture<'_, Vec<PrivateEffect>> {
            unavailable()
        }
    }

    fn ctx<'a>(
        instance: &'a Instance,
        session: &'a Session,
        budgets: &'a Budgets,
        version: &'a str,
        call_cancel: Option<&'a CancellationToken>,
    ) -> RequestContext<'a> {
        RequestContext {
            instance,
            session,
            who: &session.caller,
            budgets,
            client_version: version,
            call_cancel,
        }
    }

    fn queued_reply() -> (mpsc::Receiver<Result<RawJson, McpError>>, u64) {
        let reply = RawJson::parse(r#"{"jsonrpc":"2.0","id":7,"result":{}}"#).expect("reply JSON");
        let (tx, rx) = mpsc::channel(8);
        tx.try_send(Ok(reply)).expect("queue the reply");
        (rx, 7)
    }

    #[tokio::test]
    async fn queued_reply_wins_over_call_cancellation() {
        let services: Arc<dyn Services> = Arc::new(TestServices);
        let caller = HookCx::for_test(Arc::clone(&services), SessionId::new_v7(), None).caller;
        let session = Session {
            services,
            caller,
            cancel: CancellationToken::new(),
            declarations: Mutex::new(HashMap::<Key, DeclaredServer>::new()),
            instances: Mutex::new(HashMap::<Key, Arc<Instance>>::new()),
            publishing: Mutex::new(()),
        };
        let key = Key {
            session: SessionId::new_v7(),
            skill: "test".into(),
            server: "fixture".into(),
        };
        let endpoint = dal_core::ext::McpEndpoint::parse("https://mcp.example.invalid/mcp")
            .expect("fixture endpoint");
        let instance = Instance::new(
            key,
            Arc::new(dal_core::ext::McpServerDecl::Http { endpoint }),
            CancellationToken::new(),
        );
        let budgets = Budgets::default();
        let version = "test-client".to_owned();
        let fired = CancellationToken::new();
        fired.cancel();
        for _ in 0..32 {
            let (mut events, id) = queued_reply();
            let waiting = ctx(&instance, &session, &budgets, &version, Some(&fired));
            let reply = await_reply(id, &mut events, waiting)
                .await
                .expect("a reply queued before cancellation returns Ok");
            assert!(reply.as_str().contains("\"id\":7"));
        }
        let live = CancellationToken::new();
        let (mut events, id) = queued_reply();
        await_reply(
            id,
            &mut events,
            ctx(&instance, &session, &budgets, &version, Some(&live)),
        )
        .await
        .expect("a queued reply returns Ok while the call lives");
        let (_held, mut empty) = mpsc::channel::<Result<RawJson, McpError>>(8);
        let cancelled = await_reply(
            7,
            &mut empty,
            ctx(&instance, &session, &budgets, &version, Some(&fired)),
        )
        .await;
        assert!(
            matches!(cancelled, Err(TransportError::Cancelled)),
            "cancellation with no queued reply still cancels"
        );
    }
}
