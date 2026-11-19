#### Protocol step 9. The host owns live settings

- `internal/agent`: `Host` fields `settingsMu sync.Mutex`, `live config.Config` (set in `NewHost`); `Host.Config` (`access.go:17`) returns `live` under the lock; `func (h *Host) WriteSettings(edits ...config.SettingEdit) error` runs `live.With`, then `config.WriteSettings`, then swaps `live`; all three run under `settingsMu`. Replace every read of `h.cfg.Config` outside `NewHost` with `h.Config()`: `compaction.go:125,164`, `host.go:142,255`, `request.go:124,340-352`, `spawn.go:99`, `tools.go:390-391`.
- `internal/rpc/serve_thread.go:207-250`: delete `settingOverrides`, `hostSettings`, `overridesOf`, `set`, `apply`, `currentConfig`; callers (`serve_misc.go:58,84`, `serve_thread.go:202,289`, `serve_turn.go:143`) use `s.host.Config()`; `configValueWrite` (`serve_misc.go:140`) keeps its body until Step 10 routes it through `s.host.WriteSettings`.
- Test `internal/agent` `TestHostWriteSettingsIsLive`: `h.WriteSettings(config.SettingEdit{Key: "reasoning", Value: "high"})` changes `h.Config()` at once, and an edit that fails validation leaves `h.Config()` and the file unchanged. Fails before: `Host.WriteSettings` does not exist. The request-level test is in Step 10.
- Approval and sandbox are not live for an existing thread. The gate reads `w.settings.Approval` (`internal/agent/gate.go:92`) and `w.settings.Sandbox` (`internal/agent/gate.go:227`). `worker.use` (`internal/agent/turn.go:44-45`) sets `w.settings` from `pathSettings` (`turn.go:32-42`), which reads only the journal: the `SessionCreated` values (`internal/agent/event/event.go:117-118`) and each `UserMessage` value that is set (`event.go:195-196`, applied at `turn.go:37-38`). A saved change reaches new threads (`internal/rpc/serve_thread.go:289-292`) and a turn whose request sends `approvalPolicy` or `sandboxPolicy` (`internal/rpc/serve_turn.go:82,86`). An empty value is ignored (`internal/rpc/serve_thread.go:37-40`, `internal/rpc/serve_turn.go:71-72`).
- Docs: the live-settings paragraph in `docs/spec/config.md`, after its key table.
- Lands after: Step 8 (`config.WriteSettings`, `Config.With`).

Anchor: David Parnas.

