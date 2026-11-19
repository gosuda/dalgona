#### Protocol step 15. `mcpServerStatus/list`

- `internal/agent`: `type MCPServerStatus struct { Name, Status, Error string; Tools []tool.Spec }`; `HostConfig.MCPStatus func(ctx context.Context, cwd string) []MCPServerStatus`; `func (h *Host) MCPStatus(ctx context.Context, cwd string) []MCPServerStatus` (nil hook returns nil).
- `internal/app/app.go:50-57` `Batteries.MCPStatus`; pass it in `NewHost` config (`app.go:88-94`).
- `internal/app/battery/battery.go`: `servers` (line 363) gains `states map[string]serverState`, where `serverState` is `struct{ Status, Error string }` and `Status` is `starting`, `connected` or `failed` set in `startServers` (line 372); `func (s *servers) statuses() []agent.MCPServerStatus` (one entry per server, sorted by name); `func (p *projects) status(ctx context.Context, cwd string) []agent.MCPServerStatus` joins `p.native` and `p.byCwd[config.CanonicalWorkspace(cwd)]` (a missing entry adds nothing); the `app.Batteries` literal of `(*Daemon).Load` (`battery.go:73-76`) sets `MCPStatus: projects.status`.
- `internal/rpc/serve_mcp.go`: `(*server).mcpServerStatusList`; cwd: `info.Cwd` from `s.info(threadId)` (`serve_thread.go:140`) when `threadId` is set, else `s.host.Config().Cwd`; tool names drop the `<server>__` prefix; `limit` default 100, maximum 1000; `cursor` is an offset string as in `thread/list`.
- `responseSchemas["mcpServerStatus/list"] = "v2/ListMcpServerStatusResponse.json"`.
- Tests: `internal/rpc` `TestMCPServerStatusList` (stub hook: one connected server with one tool, one failed server; `dal`-style nil hook gives `{"data":[]}`); `internal/app/battery` `TestMCPStatusReportsFailedServer` (bogus command gives `failed` with an error). Fails before: `-32601`.
- Docs: `protocol.md`, and `docs/spec/mcp.md` under `## Lifecycle`: the three `runtimeStatus` values and their meaning. The stderr-tail sentence of cross-phase decision 40 is not this step's.

Anchor: Herbert Simon.

