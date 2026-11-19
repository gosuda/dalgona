#### Protocol step 3. Context window and token usage on resume

- `event.AssistantMessage.ContextWindow int` (`context_window`, optional, MUST NOT be negative): `event.go`, `assistantMessageMembers`, `decodeAssistantMessage`, `Validate`.
- `internal/agent`: field `contextWindow int` on `worker`; `worker.request` (`internal/agent/request.go:111-121`) sets it from `caps.Context`; `journalAssistant` (`request.go:212`) writes it. In-band cover messages (`internal/agent/inband.go:168,298`) leave it zero.
- `internal/rpc/serve_track.go` `assistantMessage` (line 141): `ModelContextWindow: e.ContextWindow`; replace the tracker field `total TokenUsageBreakdown` with `usage ThreadTokenUsage`. `assistantMessage` sets `t.usage.ModelContextWindow` to `e.ContextWindow` when that is positive (before the zero-usage return), adds the message usage to `t.usage.Total`, sets `t.usage.Last`, and sends a copy of `t.usage` as `TokenUsage`. `snapshot.usage` is set when `t.usage.Last` or `t.usage.ModelContextWindow` is non-zero.
- `internal/rpc/serve.go` `snapshot` struct gains `usage *ThreadTokenUsage`, set by `fill` from the tracker when any usage was seen.
- `ThreadStartResponse.TokenUsage *ThreadTokenUsage json:"tokenUsage,omitzero"`; `session` (`internal/rpc/serve_thread.go:185`) copies it; `ThreadStartResponse.write` writes it with the existing breakdown writer (`serve_notify.go:42-75`).
- Test `internal/rpc` `TestTokenUsageCarriesContextWindowAndSurvivesResume`: rig with `r.cfg.Config.Models = {"openai/gpt-test": {ContextWindow: 200000}}`; after one turn `thread/tokenUsage/updated.tokenUsage.modelContextWindow == 200000`; a second connection's `thread/resume` result has `tokenUsage.total.totalTokens == 27` (fake usage 20 + 7, `serve_harness_test.go:105`) and the same window. Fails before: window missing, no `tokenUsage` member.
- Docs: `events.md`, `protocol.md`.

Anchor: Claude Shannon.

