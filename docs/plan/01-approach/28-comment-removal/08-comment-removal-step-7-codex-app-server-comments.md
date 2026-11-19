#### Comment removal step 7. Codex app-server comments

Anchor: Edsger W. Dijkstra

- Files: `internal/codexapp/appserver.go`, `docs/spec/daemon.md`.
- Change: delete the comment lines of rows 46 to 48.
- Spec text, `docs/spec/daemon.md`: as a new paragraph after the paragraph that starts `` `--config <key>=<value>` ``, add these sentences in this order. Row 46: The `--config` argument MUST split at its first `=`. A `--config` argument without `=`, or with an empty key, MUST fail with exit status 2. A `--config` value MUST lose its surrounding whitespace and quote characters. A `--config` key or value that dalgona does not map MUST be ignored with a note. A later mapped `--config` value for a key MUST replace an earlier one. The `--config` split MUST follow the Codex rule in `codex-rs/utils/cli/src/config_override.rs`. Row 47: The app-server `--listen` value MUST be `stdio://` or `ws://IP:PORT`. The WebSocket address MUST be a literal IP address with a port. Any other `--listen` value MUST fail with exit status 2 before the listener starts. A non-loopback address MUST fail even when WebSocket authentication is configured. The loopback rule MUST follow Codex `codex-rs/app-server-transport/src/transport/websocket.rs`. Row 48: The app-server WebSocket listener MUST refuse a request with a non-empty `Origin` header with HTTP 403. It MUST answer a request without a valid `Authorization: Bearer <token>` header with HTTP 401. It MUST compare the bearer token digest in constant time. It MUST answer HTTP 503 while it shuts down, and HTTP 503 when it cannot dial the daemon. It MUST NOT upgrade a request that the daemon dial refused. The Origin refusal MUST follow Codex `codex-rs/app-server-transport/src/transport/websocket.rs`.
- Tests that fail before the change: none. Behaviour is pinned by `TestAppServerConfigMappingAndIgnoredFlags` (`internal/codexapp/appserver_test.go:173`), `TestAppServerRejectsInvalidArguments` (`:268`), `TestAppServerWebSocketRefusesOriginHeader` (`:490`) and `TestAppServerWebSocketRequiresBearerToken` (`:511`).
- Check: `just verify` passes. `grep -n "listen" docs/spec/daemon.md` returns the new sentences.
- Lands after: Comment removal step 6

