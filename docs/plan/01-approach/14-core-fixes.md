### Workstream: Core fixes

#### Outcome of the core-fixes workstream

A rate limit that asks for a wait over 60 seconds ends the request at once when no client is attached; with a client attached, a wait up to 300 seconds shows a countdown and a longer wait ends the request. A quota 429 and every 402 end the request at once with a clear message instead of three useless retries. Compaction never grows the context. Anthropic replays `redacted_thinking` blocks unchanged and receives `xhigh` and `max` when the model offers them. MCP tools with any name reach every provider, and the `patch` tool refuses edits that stand in for code with `// ... existing code ...`. Patch writes are pinned on Windows as on Unix, both providers on the Connect protocol share one framing package, and `dal login chatgpt --device-auth` signs in on a host without a browser.

#### Decisions of the core-fixes workstream

1. Retry-After check location. `RunRetries` (`internal/model/stream.go:445-469`) checks the wait after `retryableAttempt` passes, through `waitError` (step 1). Without an attached client, a wait above 60 seconds is refused; with one, a wait up to 300 seconds is taken and a longer one is refused. `<s>` is the wait rounded up to whole seconds. Rejected rival: a check inside `RetryDelay`. `RetryDelay` returns a duration, not an error, so it cannot stop the loop.
2. Quota codes. Seven codes count as quota: `insufficient_quota`, `credit_balance_exhausted`, `organization_spend_limit_exceeded`, `project_spend_limit_exceeded`, `organization_usage_limit_exceeded`, `usage_limit_reached`, and `usage_not_included`. A code matches only as a quoted JSON string such as `"insufficient_quota"`. Rejected rival: matching bare words, because prose such as `see insufficient_quota docs` would then turn a real rate limit terminal. The two Codex codes are included because `docs/spec/providers.md:79` already says usage-limit errors are terminal, yet `stream.go:369` retries a 429 that carries one.
3. 402. `classifyHTTPStatus` already returns `Terminal` for every 4xx other than 401, 403, 408 and 429 (`stream.go:365-375`). The step adds a test case and a spec sentence and changes no code. Rejected rival: an explicit 402 branch, which would be dead code.
4. Compaction that does not shrink. An automatic compaction that does not shrink returns "not compacted" and the turn continues; a manual or overflow compaction returns `ErrCompactionNotSmaller`. Rejected rival: failing the turn. Auto compaction runs before every step (`internal/agent/turn.go:196`), so a failure there would block the thread at every later turn. The RPC error mapping (`internal/rpc/serve_errors.go:45`) stays as it is, because this workstream does not change client interfaces. The error falls through to `internalError` (`internal/rpc/serve_errors.go:50`).
5. Where `redacted_thinking` is stored. It goes on each tool call as `RedactedThinking []string`, next to the existing `Signature`. Rejected rival: a raw list of content blocks on `assistant.message`. It would add a second replay path beside the signature path that `thinkingBlock` already uses (`anthropic.go:229-239`).
6. MCP name rule. Names are at most 64 bytes and use `[A-Za-z0-9_-]`. The first character is a letter or `_`. A name that was changed in any way gets an `_<8 hex>` suffix from SHA-256 of the original `<server>__<tool>`. Sources:
   - Chat Completions caps a function name at 64 with that character set: https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create (line 806).
   - The Responses function tool gives no limit: https://developers.openai.com/api/reference/resources/responses/methods/create (lines 5715-5717).
   - Anthropic allows `^[a-zA-Z0-9_-]{1,128}$`: https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools (line 57).
   - Gemini requires a letter or underscore first, max 128: https://docs.cloud.google.com/gemini-enterprise-agent-platform/reference/rest/Shared.Types/FunctionDeclaration.

   Rejected rival: adding the hash only on truncation. Replacing characters alone merges `a.b` and `a:b`.
7. Omission detector. A placeholder is a comment line that holds an ellipsis and whose remaining text is empty or starts with a fixed phrase. It is refused only when the old file holds fewer copies of that line. Files with extensions `.md`, `.markdown`, `.mdx`, `.rst`, `.txt` and `.adoc` are skipped. Rejected rival: a length rule that refuses a placeholder body under 20 characters. It rejects only near-empty bodies, so it misses `// ... existing code ...` inside a full function.
8. Exit wait. The new function is `runner.AwaitExit(ctx, r, id, grace)`, and it ends when the caller's context ends. `web_fetch` passes the call context, not its 35 s `remoteCtx`. That way its own fetch timeout still waits for the helper to exit, and a user interrupt does not. Rejected rival: `context.WithoutCancel(ctx)`, which keeps the 5 s stall on interrupt, the defect this step removes.
9. Connect package scope. One package, `internal/model/connectwire`, holds both the framing and the protobuf field helpers. Rejected rival: framing only. Lines 179-479 of `internal/model/cursor/wire/wire.go` hold the helpers, more than the framing at lines 15-177.
10. Windows pinned writes. This is a real implementation on `os.Root`. `go.mod` pins `go 1.27.0` (`go.mod:3`), and the Windows compile check in Verification 12 proves that every `Root` method the code uses exists. Rejected rival: an honest refusal, which would make the `patch` tool unusable on Windows.
11. Device login id. The flag lands on the existing provider id: `dal login chatgpt --device-auth`. Rejected rival: an `openai-codex` alias. `Providers()` (`internal/app/login.go:24-26`) lists `chatgpt` as one provider id, and an alias would give that provider a second id. The flag name copies Codex (`.references/codex/codex-rs/cli/src/main.rs:552`).
12. `Store.Rename` (`internal/tool/snapshot/store.go:253-259`) gets no change.
    - Each thread owns its store (`internal/agent/thread.go:116`), and the only caller is `internal/tool/patch/writer.go:430`, after publication.
    - Edits run one at a time (`internal/agent/tools.go:80-100`); only agent-class calls run in parallel, and each child has its own thread.
    - Script scopes can run calls concurrently (`internal/script/ctx.go:145-159`). The state between the two lock sections is still just `Remove(old)` followed by `Put(new)`, two valid transitions. A single lock would not order a concurrent capture against the rename either, so no reader can see a different outcome.
13. `rewriteSnapshotBlobs` (`internal/agent/tools.go:536-544`) gets no change.
    - Its only input is metadata built by `snapshot.Entry.Meta` (`internal/tool/snapshot/meta.go:13-19`). That code writes compact `,"blob":` followed by `AppendJSONString`, which escapes `"` and `\` (`jsonstring.go:17-18`). The pattern `"blob":"<hex>"` therefore matches only the real key.
    - Both digests are 64-character lowercase hex (`store.go:423-426`, `internal/journal/journal.go:574-575`), so `copy(meta, rewritten)` at `tools.go:532` copies equal lengths.
    - A chained remap would need text that masks again to a new value. Masked text holds only `{{secret:...}}` placeholders, which do not match a secret again, so masking it twice gives the same text.
14. `stdioTransport` (`internal/mcp/stdio.go:18-28`) gets no change.
    - Every use runs under `Client.mu`: `call` (`internal/mcp/mcp.go:111-118`), `Close` (`:98-108`), and restart through `callLocked` (`:120-138`), which calls `restartStdioLocked` (`:179-191`). `New` initializes the transport before it publishes the client (`:51-74`).
    - The goroutines in `writeLine` and `readLine` capture only local copies (`stdio.go:155-158`, `:182-185`) and never touch transport fields.
    - `stderrTail` has its own mutex (`stdio.go:34-58`).
15. Attended wait. A client is attached when the thread has a subscriber, the test `offerApproval` already uses (`internal/agent/gate.go:292-293`). With a client, a wait above 60 seconds sends its remaining time each second as the transient update `UpdateRetryWait` and the notification `dalgona/retry/wait` (step 1a). Rejected rival: one `model.retry` journal event per second. The journal is append-only (`docs/spec/journal.md`), so a countdown would fill it.

#### Core-fixes step 1. Retry-After over 60 s stops the request (product decision 13)

- `internal/model/stream.go`:
  - Keep `const maxRetryAfter = 5 * time.Minute` at line 539: it is the limit for an attended wait. Add `const maxUnattendedRetryAfter = time.Minute`, the limit with no client attached.
  - `RetryDelay` (`:541-551`): replace `return min(retryAfter, maxRetryAfter)` with `return retryAfter`.
  - Add `type RetryWatch struct { Attended func() bool; Tick func(remaining time.Duration) }`, `func WithRetryWatch(ctx context.Context, watch RetryWatch) context.Context` and `func retryWatchFrom(ctx context.Context) RetryWatch`. The agent sets the watch in step 1a. With no watch, no client is attached and nothing is ticked.
  - Add `func waitError(ctx context.Context, wait time.Duration) error`. The limit is `maxUnattendedRetryAfter`. When `wait` is above it and the watch's `Attended` is set and returns true, the limit is `maxRetryAfter`. A `wait` within the limit returns nil. Otherwise it returns `errors.New("rate limited for " + strconv.FormatInt(seconds, 10) + " s, over the " + strconv.FormatInt(int64(limit/time.Second), 10) + " s wait limit")`, where `seconds := int64((wait + time.Second - 1) / time.Second)`.
  - In `RunRetries` (`:445-469`), right after `if !retryableAttempt(result, verdict, index) { return result }`, insert `if err := waitError(ctx, result.RetryAfter); err != nil { result.Err = err; return result }`. The result keeps its `Status` and `Body`.
  - Change `retrySleep` to wait in steps of at most one second. When `delay` is above `maxUnattendedRetryAfter` and the watch's `Tick` is set, call `Tick(remaining)` before each step, and call `Tick(0)` once on every exit. A shorter delay makes no `Tick` call, so ordinary backoff stays silent. Each step calls `sleep(ctx, min(remaining, time.Second))`. `retrySleep` checks `ctx.Err()` before each step and returns it when set, so a sleep hook that ignores the context cannot keep the wait alive. The default `sleepContext` returns `ctx.Err()` as soon as the context ends. That context is the turn context: `turn/interrupt` calls `Thread.InterruptTurn` (`internal/rpc/serve_turn.go:218`), which calls `actor.interrupt` (`internal/agent/entry.go:152-157`), which calls `a.cancel(errInterrupted)`; `actor.spawn` builds that context with `context.WithCancelCause` (`internal/agent/turn.go:66-70`). `RunRetries` then sets `result.Err` to that error and returns at once, and the deferred `Tick(0)` runs.
- Keep `internal/model/stream_retrydelay_test.go`. Change its last assertion to `RetryDelay(1, time.Hour, nil) == time.Hour`, because `RetryDelay` no longer caps and `RunRetries` refuses the wait first. Its 90 s and 60 s cases stay: an attended wait honours them.
- Failing tests:
  - `internal/model` `TestRunRetriesStopsOverTheRetryAfterLimit` in `stream_retry_test.go`. Table: `90*time.Second` gives `rate limited for 90 s, over the 60 s wait limit`; `61500*time.Millisecond` gives `rate limited for 62 s, over the 60 s wait limit`. The attempt returns `AttemptResult{Status: 429, RetryAfter: <wait>, Body: []byte(`{"error":{"type":"rate_limit_error"}}`), Err: errors.New("openai: http 429")}`. Assert one attempt, zero `Sleep` and zero `Tick` calls, the exact message, `Status == 429` and an unchanged body. The table has no watch, so no client is attached. With a `WithRetryWatch` whose `Attended` returns true, `301*time.Second` gives `rate limited for 301 s, over the 300 s wait limit`, with the same assertions.
  - `internal/model` `TestRunRetriesWaitsAtTheRetryAfterLimit`: `RetryAfter: 60*time.Second`, then success. Assert that the `Sleep` steps add up to exactly 60 s, that `Tick` is never called without a watch, and that there are two attempts. This passes before and after; it guards the boundary.
  - `internal/model` `TestRunRetriesWaitsWhileAttended`: a watch with `Attended` true, `RetryAfter: 90*time.Second`, then success. Assert that the `Sleep` steps add up to 90 s with no step above one second, that the `Tick` values start at 90 s and end with `0`, and that there are two attempts. Before the change there is no `Tick`, so the test fails.
  - `internal/model` `TestRunRetriesStopsWhenInterruptedWhileWaiting`: a watch with `Attended` true, `RetryAfter: 90*time.Second`, and the real `sleepContext` as `Sleep`, with a context that the `Tick` callback cancels on its second call (`context.WithCancelCause`). Assert that `RunRetries` returns within 5 s (a `select` with a timer), one attempt, that `result.Err` is `context.Canceled`, and that the last `Tick` value is `0`.
  - `internal/model/openai` `TestRetryAfterOverLimitStopsTheRequest` in `openai_test.go`. Server: `w.Header().Set("Retry-After", "120"); w.WriteHeader(429)`. Use `fastRetry` with an `OnRetry` counter. Assert the last event is `model.Error` with `Message == "rate limited for 120 s, over the 60 s wait limit"`, `Status == 429`, and zero retries.
- Spec: `docs/spec/providers.md:81`. Replace the `Retry-After` sentence with: "A valid `Retry-After` value up to 60 seconds MUST replace the delay. With a client attached, a value above 60 seconds and up to 300 seconds MUST wait, and the wait MUST be reported through `dalgona/retry/wait`. A larger value MUST stop the request with `rate limited for <s> s, over the <limit> s wait limit`, where `<limit>` is 300 with a client attached and 60 without one, and `<s>` is the wait rounded up to whole seconds. The error MUST keep the HTTP status and body."
- Anchor: Tony Hoare.

#### Core-fixes step 1a. Attended waits show a countdown (product decision 13)

- `internal/agent/turn.go`: at the top of `func (w *worker) loop(ctx context.Context) outcome`, set `ctx = model.WithRetryWatch(ctx, model.RetryWatch{Attended: ..., Tick: ...})`.
  - `Attended` reports `attached, err := ask(w.t, func(a *actor) bool { return len(a.subs) > 0 })` and returns `err == nil && attached`. This is the test `offerApproval` uses (`internal/agent/gate.go:292-293`).
  - `Tick(remaining)` calls `ask(w.t, func(a *actor) bool { a.publish(Update{Kind: UpdateRetryWait, Thread: a.t.id, Turn: w.turnID, Wait: remaining}); return true })` and ignores the error.
- `internal/agent/types.go:132-140`: add `UpdateRetryWait` after `UpdateExecOutput`. In `Update` (`types.go:155`), add `Turn string` and `Wait time.Duration`, read only for `UpdateRetryWait`.
- `internal/rpc/serve.go:333-334`: in the `Update` switch, beside the `UpdateExecOutput` case, add `case agent.UpdateRetryWait: w.tracker.retryWait(update.Turn, update.Wait)`.
- `internal/rpc/serve_track.go`: add the tracker method `retryWait(turn string, wait time.Duration)` beside `execOutput`. Define `RetryWaitNotification`, with the JSON names `threadId`, `turnId` and `remainingMs`, next to the other `dalgona/` notification types. It calls `t.sink.notify("dalgona/retry/wait", func() writer { return RetryWaitNotification{ThreadID: t.threadID, TurnID: turn, RemainingMS: wait.Milliseconds()} })`, the form `dalgona/advisor/note` uses (`serve_track.go:395-397`).
- Schema: add `internal/rpc/testdata/dalgona-schema/dalgona.retry.wait.json` (draft-07, `threadId` and `turnId` strings, `remainingMs` integer). The protocol workstream, Step 1 requires one schema file for each `dalgona/` notification. Add `dalgona/retry/wait` to the `dalgonaNotifications` list in `internal/rpc/schema_dalgona_test.go`.
- Neither the update nor the notification is journaled, so no journal kind is added.
- Spec: `docs/spec/protocol.md`: add one row for `dalgona/retry/wait` to the protocol methods table, beside the other `dalgona/` rows. This step adds only that row. The protocol workstream owns the rest of the file. `docs/spec/providers.md`: say that a client shows `rate limited · retrying in <m>m <ss>s · esc stops` for the remaining time, where `<m>` is a whole number of minutes and `<ss>` is the remaining seconds with two digits (the duration form of cross-phase decision 33), that Esc sends `turn/interrupt` (`internal/tui/model.go:202-203`, `internal/tui/commands.go:191-192`), and that a client honouring reduced motion (`DAL_NO_MOTION` in the terminal, `prefers-reduced-motion` in the browser; cross-phase decision 39) shows the countdown as static text.
- Failing tests:
  - `internal/agent` `TestRetryWaitReachesAttachedClient` in a new `retry_wait_test.go`. The harness sets `Sleep` to a no-op (`internal/agent/harness_test.go:263`). The model answers 429 with `Retry-After: 90`, then success. A subscriber from `thread.Subscribe()` (`internal/agent/thread.go:508`) must receive `UpdateRetryWait` updates whose first `Wait` is 90 s and whose last is 0, and the turn completes. Before the change there is no such update kind.
  - `internal/agent` `TestRetryWaitStopsWithoutSubscriber`: no subscriber, `Retry-After: 90`. The turn fails with `rate limited for 90 s, over the 60 s wait limit`, and no `UpdateRetryWait` is published.
  - `internal/rpc` `TestRetryWaitNotification` in a new `serve_retry_test.go`: a client receives `dalgona/retry/wait` with `threadId`, `turnId` and `remainingMs`, and a final `remainingMs` of 0.
  - `internal/agent` `TestInterruptEndsRetryWait` in `retry_wait_test.go`: the model answers 429 with `Retry-After: 90` and a subscriber is attached. After the first `UpdateRetryWait`, the test calls `thread.Interrupt()`. Assert that the turn ends interrupted within 2 s, that the model received one request, and that the last `UpdateRetryWait` has `Wait` 0. The test runs with the real `sleepContext`. Fails before: `UpdateRetryWait` does not exist.
- Lands after: Step 1 of this workstream, and the protocol workstream, Step 1 (it creates the `dalgona-schema` directory and the walker that this step's schema file joins).
- Anchor: Edward Tufte.

#### Core-fixes step 2. Quota 429 and 402 are terminal (product decision 15)

- `internal/model/stream.go`:
  - Add `var quotaCodes = [...]string{"insufficient_quota", "credit_balance_exhausted", "organization_spend_limit_exceeded", "project_spend_limit_exceeded", "organization_usage_limit_exceeded", "usage_limit_reached", "usage_not_included"}`.
  - Add `func isQuota(body []byte) bool`. It lowercases the body once and returns true when it contains `"` + code + `"` for any code.
  - In `Classify` (`:349-363`), the `classifyHTTPStatus` branch (`:353-355`) becomes `if verdict := classifyHTTPStatus(status); verdict != 0 { if verdict == Retry && status == http.StatusTooManyRequests && isQuota(body) { return Terminal }; return verdict }`. Change `if hasRetryableCode(body)` (`:359`) to `if hasRetryableCode(body) && !isQuota(body)`. The quota check names 429 only, so a 5xx or 408 that names a quota code stays retryable. No separate helper: each check stays in `Classify`.
  - `classifyAttempt` (`:478-496`) passes the error text as the body when there is no body, so a provider message such as `openai: http 429: {...}` is covered as well.
- Failing tests:
  - `internal/model` `TestClassify` (`model_test.go:5-34`), new cases:
    - `{name: "quota rate limit", status: 429, body: `{"error":{"type":"insufficient_quota","code":"insufficient_quota"}}`, want: Terminal}`
    - `{name: "codex usage limit", status: 429, body: `{"error":{"type":"usage_limit_reached"}}`, want: Terminal}`
    - `{name: "in-band quota", status: 200, body: `{"error":{"code":429,"type":"insufficient_quota"}}`, want: Terminal}`
    - Guards that already pass: `{name: "payment required", status: 402, body: `{"type":"error","error":{"type":"billing_error"}}`, want: Terminal}`; `{name: "quota word in prose", status: 429, body: `{"error":{"message":"see insufficient_quota docs"}}`, want: Retry}`; `{name: "workspace limit is not a quota code", status: 429, body: `{"error":{"type":"workspace_owner_usage_limit_reached"}}`, want: Retry}`.
  - `internal/model/openai` `TestQuotaRateLimitDoesNotRetry`: the server answers 429 with `{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}`. Assert zero retries and a final `model.Error` with `Status == 429`.
- Spec: `docs/spec/providers.md:79`. After the sentence `Usage-limit and other errors MUST be terminal.` add: "A 429 whose body names a quota code as a JSON string MUST be terminal. The quota codes are `insufficient_quota`, `credit_balance_exhausted`, `organization_spend_limit_exceeded`, `project_spend_limit_exceeded`, `organization_usage_limit_exceeded`, `usage_limit_reached`, and `usage_not_included`. A 402 MUST be terminal." In the in-band sentence, `An HTTP 200 body or stream error with a numeric 429 or 5xx code MUST be retryable.`, add `unless it names a quota code` before the full stop.
- Anchor: Edsger W. Dijkstra.

#### Core-fixes step 3. Anthropic passes `xhigh` and `max` through (product decision 14)

- `internal/model/anthropic/anthropic.go:352-361` `anthropicEffort`: `case "minimal", "low": return "low"`, then `case "medium", "high", "xhigh", "max": return reasoning`, then `default: return "high"`.
- The model's advertised levels are enforced before the encoder runs: `ClampEffort` (`internal/model/presets.go:103-112`) clamps the request to `caps.Efforts` (`internal/agent/request.go:119`), and those come from `supportedEfforts` (`anthropic.go:390-412`).
- Failing test: `internal/model/anthropic` `TestEncodePassesXHighAndMaxEffort` in `anthropic_test.go`. Table: `xhigh` gives `xhigh`, `max` gives `max`, `minimal` gives `low`. Capture the body as `TestRequestBody` does (`:61-85`) and decode it with the existing `OutputConfig.Effort` test struct (`:333-335`). Before the change, `xhigh` and `max` produce `high`.
- Spec: `docs/spec/providers.md:11`. Replace the effort sentence with: "Effort MUST map minimal and low to `low` and MUST pass `medium`, `high`, `xhigh`, and `max` through unchanged, after the agent clamps the request to the model's advertised levels."
- Anchor: Dieter Rams.

#### Core-fixes step 4. Keep and replay Anthropic `redacted_thinking` (product decision 15)

- `internal/model/model.go:35-42` `ToolCall`: add `RedactedThinking []string`.
- `internal/agent/event/event.go:62-70` `ToolCall`: add `RedactedThinking []string`.
  - Extract `func validateToolCall(i int, call ToolCall) error` from `AssistantMessage.Validate` (`:282-299`). It also rejects an empty entry with `tool_calls[%d]: redacted_thinking[%d] must not be empty`. This keeps both functions under the cyclop limit of 9 (`.golangci.yml:29-30`).
- `internal/agent/event/encode.go:162-171` `toolCallValue`: when `len(e.RedactedThinking) > 0`, append `field("redacted_thinking", arrayValue(e.RedactedThinking, stringValue))`.
- `internal/agent/event/decode.go:126-138` `decodeToolCall`: read `r.array("redacted_thinking", ...)` with `decodeString`, as for `call_ids` at `decode.go:232-235`.
- Copy the field in `internal/agent/request.go:197-201` (`apply`) and `internal/agent/context.go:199-201` (`transcript.assistant`).
- `internal/model/anthropic/decode.go`:
  - Add `redacted []string` to `responseState` (`:14-25`) and `Data string` to `block` (`:42-47`).
  - In `readBlock`, add `case "data": block.Data, err = model.ReadString(decoder)`.
  - `startBlock` (`:131-142`): when `block.Type == "redacted_thinking"`, append `block.Data` to `s.redacted` and return.
  - `finishTool`: after `tool.Signature = s.signature` (`:190`), set `tool.RedactedThinking = slices.Clone(s.redacted)`.
- `internal/model/anthropic/anthropic.go`:
  - Replace `thinkingBlock` (`:229-239`) with `func replayFor(message model.Message, thinking bool) (string, []string)`. It returns the first non-empty `Signature` only when `message.Reasoning != ""`. It returns the first non-empty `RedactedThinking` whenever `thinking` is true, even when `message.Reasoning` is empty, because the redacted test case has no reasoning. It returns nothing when `!thinking`.
  - `encodeAssistant` (`:207-227`): skip the message only when text, calls, signature and redacted are all empty. Write the thinking block when there is a signature. Then loop over the redacted entries in order and write `{"type":"redacted_thinking","data":<data>}` inline for each, then text, then `tool_use`. No helper is added for that one object write.
- Failing tests:
  - `internal/model/anthropic` `TestStreamKeepsRedactedThinking` in `signature_test.go`. Inline SSE: `message_start`; `content_block_start` index 0 with `{"type":"redacted_thinking","data":"enc-1"}`; `content_block_stop` 0; a `tool_use` block at index 1 with `input_json_delta` `{"path":"a"}`; `message_delta` with `stop_reason` `tool_use`; `message_stop`. Assert `calls[0].RedactedThinking` equals `["enc-1"]`.
  - `internal/model/anthropic` `TestEncodeReplaysRedactedThinkingUnchanged`, with `Reasoning: "high"` and an assistant message with `Reasoning: ""` and a call carrying `RedactedThinking: []string{"enc-1"}`. The body contains `{"type":"redacted_thinking","data":"enc-1"}` before `"type":"tool_use"`. A second case with signed thinking `why`/`sig-9` puts the thinking block first.
  - Guard: `TestEncodeOmitsRedactedThinkingWhenDisabled` (no `Reasoning`; body has no `redacted_thinking`).
  - `internal/agent/event` `TestToolCallRedactedThinkingRoundTrip` in `strictness_test.go`, mirroring `TestToolCallSignatureRoundTrip` (`:85-92`), plus `redacted_thinking:[""]` rejected by `Validate`.
  - `internal/agent` `TestTranscriptRebuildsRedactedThinking` and `TestApplyKeepsRedactedThinking` in `signature_test.go`. The second calls `(&worker{}).apply(&reply{}, model.ToolCall{ID: "c", Name: "read", Args: jsontext.Value(`{}`), RedactedThinking: []string{"enc-1"}})`. `(&worker{})` is enough: the `model.ToolCall` case (`internal/agent/request.go:197-201`) reads no worker field.
- Spec:
  - `docs/spec/events.md:26`: each tool call gains "optional `redacted_thinking` (opaque provider strings replayed unchanged)".
  - `docs/spec/providers.md:57`: add "An Anthropic `redacted_thinking` block MUST keep its `data` string on every tool call of that response. When thinking is enabled, the encoder MUST replay each one unchanged as `{\"type\":\"redacted_thinking\",\"data\":...}` after the thinking block and before text and `tool_use`."
- Anchor: David Parnas.

#### Core-fixes step 5. Compaction must shrink (product decision 15)

- `internal/agent/compaction.go`:
  - Add the constant `notSmallerText = "compaction: the summary is not smaller than the context it replaces"` and `var ErrCompactionNotSmaller = errors.New(notSmallerText)`.
  - In `compact`, after computing `after` and before building `marker` (`:175-179`): `if after >= before { w.t.host.logOnce(fmt.Sprintf("%s\x00%s\x00%d", notSmallerText, w.t.id, cv.lastMarker), notSmallerText, "thread", w.t.id, "tokens_before", before, "tokens_after", after); return fmt.Errorf("%w (%d tokens before, %d after)", ErrCompactionNotSmaller, before, after) }`. No marker is appended.
  - `autoCompact` (`:122-134`): `if errors.Is(err, ErrNothingToSummarize) || errors.Is(err, ErrCompactionNotSmaller) { return false, nil }`.
  - `recoverOverflow` and manual `compact` (`internal/agent/entry.go:357`) return the error. An automatic compaction that does not shrink records its `lastMarker` and entry count on the thread. While both still match, `autoCompact` returns `false, nil` before it calls `compact`, so the summarizer runs once for each context growth.
- No decode-time validation is added. Old journals may already hold markers that grew, and recovery must still read them (`docs/spec/recovery.md`).
- Failing tests, both in `internal/agent` `compaction_test.go`:
  - `TestManualCompactionThatDoesNotShrinkAppendsNoMarker`: `CompactionKeepTokens = 1`. Push `text("first")`, `text("second")`, `text(strings.Repeat("s", 40000))`; submit "one" and "two", waiting for idle each time. `err := thread.Compact(t.Context())` must satisfy `errors.Is(err, ErrCompactionNotSmaller)`, and `h.events(thread.ID())` must hold no `event.Compaction`.
  - `TestAutoCompactionThatDoesNotShrinkKeepsTheTurn`: `CompactionKeepTokens = 1`, `CompactionThreshold = 1e-9`. Push `text(strings.Repeat("s", 40000))` (consumed as the summary on the first step) and `text("first")`; submit "one". The turn status must be `statusCompleted`, there must be no `event.Compaction`, `h.model.count() == 2`, and the logs must contain `msg="compaction: the summary is not smaller than the context it replaces"`. With `CompactionThreshold = 1e-9`, `ShouldCompact` (`internal/agent/compact/compact.go:41-47`) is true whenever the context holds tokens, so the first step compacts. `compact.Cut` (`:49-59`) picks the cut. The test assumes the first model request is the summary; the Step 5 contingency checks it.
- Spec: `docs/spec/journal.md:72`, `## Compaction`. Add: "The agent MUST NOT append a compaction whose `tokens_after` is not smaller than `tokens_before`. An automatic compaction that does not shrink MUST leave the turn running without a marker. A manual or overflow compaction MUST fail with `compaction: the summary is not smaller than the context it replaces`."
- Anchor: Claude Shannon.

#### Core-fixes step 6. Provider-safe MCP tool names (product decision 15)

- New file `internal/mcp/name.go`:
  - `const maxToolName = 64`.
  - `func toolName(server, tool string) string`: build `joined := server + "__" + tool` and `safe := safeToolName(joined)`. If `safe == joined && len(safe) <= maxToolName`, return `safe`. Otherwise take `sum := sha256.Sum256([]byte(joined))`, set `suffix := "_" + hex.EncodeToString(sum[:4])`, and return `safe[:min(len(safe), maxToolName-len(suffix))] + suffix`.
  - `func safeToolName(name string) string`: if the first rune is not an ASCII letter or `_`, write a leading `_`. Write each rune in `[A-Za-z0-9_-]` as is and every other rune as one `_`.
- `internal/mcp/tool.go:76`: `Name: toolName(c.cfg.Name, name)`. `serverTool.name` keeps the original, so `tools/call` still sends it (`tool.go:92`). The RPC item still splits the provider name at its first `__` (`internal/rpc/serve_track.go:224`; `internal/rpc/serve_items.go:168` tests for `__`), so an `mcpToolCall` item shows the rewritten server or tool name when the rule changed it. This workstream accepts that and adds no wire or journal field.
- Failing test: `internal/mcp` `TestToolNameIsProviderSafe` in a new `name_test.go` (`package mcp`).
  - Every output matches `^[A-Za-z_][A-Za-z0-9_-]{0,63}$`.
  - `("web","echo")` returns `web__echo` unchanged.
  - `("plug:srv","echo")`, `("1password","x")` and a 100-byte tool name are all changed and end in `_` plus 8 hex digits.
  - `("s","a.b")` and `("s","a:b")` differ, and repeated calls are equal.
  - Before the change, `plug:srv__echo` fails the pattern (plugin servers carry `<plugin>:` per `docs/spec/discovery.md:35`).
- Spec:
  - `docs/spec/mcp.md` "Tool discovery": replace the naming sentence with: "Each server tool MUST become a tool named `<server>__<tool>`. A name with a character outside `[A-Za-z0-9_-]`, a first character other than a letter or `_`, or more than 64 bytes MUST be rewritten. Each bad character becomes `_`, a bad first character gains a leading `_`, and the name is cut to 55 bytes and followed by `_` and the first 8 hex digits of SHA-256 of the original `<server>__<tool>`. `tools/call` MUST send the original tool name."
  - `docs/spec/protocol.md:106`: add "after the MCP name rule" to the `<server>__<tool>` text. This step adds only that phrase. The protocol workstream owns the rest of the file.
- Anchor: Otl Aicher.

#### Core-fixes step 7. Refuse omission placeholders in edits (product decision 15)

- New file `internal/tool/patch/omission.go`:
  - `var omissionPhrases = [...]string{"rest of", "existing code", "existing implementation", "existing methods", "remaining code", "remaining implementation", "remaining methods", "unchanged", "previous code", "same as before", "code omitted", "implementation omitted", "omitted", "other methods", "keep existing", "keep the rest", "etc"}`.
  - `var proseExtensions = [...]string{".md", ".markdown", ".mdx", ".rst", ".txt", ".adoc"}`.
  - `func checkOmissions(files []*stagedFile) error`: skip files whose `deleted` field is set, and skip prose extensions, then call `omissionLine(file.before, file.after)`. When it finds a line, return `fmt.Errorf("patch: %s line %d: %q stands in for omitted code; write the complete text. Nothing was written.", file.path, line, text)`.
  - `func omissionLine(before, after []byte) (int, string, bool)`: count placeholder lines (trimmed) in `before`, then walk `after` lines 1-based, decrementing the count per placeholder line. The first line whose count goes below zero is returned.
  - `func placeholderLine(line string) bool`: `commentText` must succeed, the text must contain `...` or `…`, and the normalized text must be empty, equal a phrase, or start with phrase + " ".
  - `func commentText(trimmed string) (string, bool)`: try the prefixes `<!--` (strip a trailing `-->`), `/*` (strip `*/`), `(*` (strip `*)`), `//`, `#`, `--` and `;` in that order.
  - Normalize the text inside `placeholderLine`: lowercase it, replace `.`, `…`, `(`, `)`, `[` and `]` with spaces, and join `strings.Fields` with one space. No separate function.
- `internal/tool/patch/stage.go:53-72` `stage`: after `stageGroups` succeeds, `if err := checkOmissions(files); err != nil { closeFiles(files); return stagedBatch{}, err }`. This runs inside `Plan`, before any write, for every edit style. `patch.PlanReplace`, which the code-rewrite workstream adds for the `rewrite` tool, calls `checkOmissions` on its staged files in the same way, so a rewrite that leaves a stand-in line is refused with the same text.
- Failing test: `internal/tool/patch` `TestPatchRefusesOmissionPlaceholder` in `omission_test.go`. The fixture is `a.go` = `package a\n\nfunc f() {\n\treturn\n}\n`. Through `runPatch(t, env, Replace, ...)`, the replacements `func f() {\n\t// ... existing code ...\n}`, `// rest of method ...` and `/* ... */` each fail with `stands in for omitted code` and leave the file unchanged (`readFixture`). An `apply_patch` add of `b.py` with `+# ... rest of implementation` also fails and creates no file.
- False-positive guard tests, in the same file and passing before and after:
  - `TestPatchKeepsPlaceholderAlreadyInFile`: the old file holds `\t// ... existing code ...`, and an edit elsewhere succeeds.
  - `TestPatchAcceptsOrdinaryEllipsisComments`: `// wait for the server...`, `// TODO ...` and `// etcd config ...` succeed, and `notes.md` with a fenced `// ... existing code ...` succeeds.
- Spec: `docs/spec/edit-styles.md`. Add `## Omission placeholders` before `## Planning and recovery` and describe the comment prefixes, the ellipsis rule, the phrase list, the count guard, the prose extensions and the exact error.
- Anchor: Richard Feynman.

#### Core-fixes step 8. One exit wait that honours the caller's context

- New file `internal/runner/await.go`:
  - `var ErrExitTimeout = errors.New("subprocess did not exit")`.
  - `func AwaitExit(ctx context.Context, r Runner, id string, grace time.Duration) error`:
    - Create `waitCtx, cancel := context.WithTimeoutCause(ctx, grace, ErrExitTimeout)` and loop.
    - Each pass calls `out, err := r.Read(waitCtx, id, 0)` and returns nil when `err == nil && out.Exit != nil`.
    - When `waitCtx.Err() != nil`, return `context.Cause(ctx)` if `ctx.Err() != nil`, else `errors.Join(ErrExitTimeout, lastErr)`. Otherwise keep `lastErr = err` and wait 20 ms on a timer in a `select` with `waitCtx.Done()`.
- `internal/tool/search/search.go`: delete `exitGrace` and `waitExit` (`:103-121`). At `:62` call `runner.AwaitExit(ctx, env.Runner, id, 5*time.Second)` and wrap the result with `fmt.Errorf("search: wait for subprocess exit: %w", waitErr)`.
- `internal/tool/webfetch/fetch.go`: delete `errRemoteExitTimeout` (`:30`), `remoteExitGrace` and `waitRemoteExit` (`:455-474`). At `:397` call `runner.AwaitExit(ctx, env.Runner, id, 5*time.Second)` with the call `ctx`, not `remoteCtx`, and wrap it as `web_fetch: wait for subprocess exit: %w`. Change `kill_test.go:88` to `errors.Is(runErr, runner.ErrExitTimeout)`.
- Failing test: `internal/tool/webfetch` `TestRemoteExitWaitEndsWithTheCallerContext` in `kill_test.go`. A wrapper around `failReadRemoteRunner{Runner: &remoteRunner{output: runner.Output{Alive: true}}, remaining: 1}` whose `Signal` calls `cancel()` on the `Run` context before delegating. Assert `errors.Is(runErr, context.Canceled)` and `errors.Is(runErr, errKillTestRead)`. Before the change, the wait ignores the context and returns `errRemoteExitTimeout` after 5 s.
- Unit tests: `internal/runner` `TestAwaitExitEndsWithTheCallerContext` and `TestAwaitExitReportsTimeout` in `await_test.go`. Use a struct embedding `Runner` whose `Read` returns `Output{Alive: true}`, and `grace = 50*time.Millisecond` for the timeout case, asserting `errors.Is(err, ErrExitTimeout)`.
- Spec: `docs/spec/runner.md:27`. Add: "The poll MUST end when the caller's context ends and MUST return that context's cause; a tool's own timeout MUST NOT end the poll."
- Anchor: John Carmack.

#### Core-fixes step 9. One shared Connect framing package

- New package `internal/model/connectwire`. It falls under the core depguard rule (`.golangci.yml:111-132`) and imports only the standard library and `google.golang.org/protobuf/encoding/protowire`, which `fast` allows.
  - `frame.go`: `Compressed`, `EndStream`, `Frame`, `AppendFrame`, `ReadFrame`, `ConnectError` and its `Error`, `TrailerError`, and the unexported trailer readers, copied verbatim from `internal/model/cursor/wire/wire.go:15-177`.
  - `proto.go`: the helpers from `cursor/wire/wire.go:179-479`, all exported by capitalizing the first letter. The devin-only helpers `setFloat32`, `setFloat64`, `appendPackedInt32`, `appendSigned32List`, `appendPresentInt32`, `setPresentInt32`, `appendFloat32` and `appendFloat64` (`devin/wire/wire.go:289-311`, `:426-455`, `:522-538`) are not shared, so they stay out of `connectwire`. `field` becomes `Field{Number int32; Kind protowire.Type; Raw []byte; Data []byte}`.
- Move the devin-only helpers into `internal/model/devin/wire/fields.go` first, unchanged except that they use the exported `connectwire` names. Then delete `internal/model/cursor/wire/wire.go` and `internal/model/devin/wire/wire.go`, and in each wire directory:
  - Run `gofmt -r '<old> -> connectwire.<New>' -w *.go` for every helper name.
  - Run `gofmt -r 'a.number -> a.Number'` and the same for `kind`, `raw` and `data`. Every lowercase selector with these names in those packages is on a `field` value (grep across `context.go`, `exec.go`, `models.go`, `chat.go`, `discovery.go`, `nested.go`).
  - Rename the type `field` to `connectwire.Field` in each wire file: first run `grep -n '\bfield\b'` in those files and rename any variable named `field`, then run `gofmt -r 'field -> connectwire.Field' -w` on them. After that, run `gofmt -r 'connectwire.Field{raw: a} -> connectwire.Field{Raw: a}' -w internal/model/devin/wire/discovery.go`; the literal is at line 310.
  - Finish with `goimports -w`.
- In `internal/model/cursor/*.go` and `internal/model/devin/*.go`, rewrite `wire.ReadFrame`, `wire.AppendFrame`, `wire.Frame`, `wire.EndStream`, `wire.Compressed`, `wire.ConnectError` and `wire.TrailerError` to `connectwire.*` (callers include `cursor/connect.go:47`, `cursor/cursor.go:108`, `cursor/session.go:66-84`, `devin/connect.go:41-82`, `devin/decode.go:65-79`, and both `*_test.go`).
- Move `TestEnvelopeSplitReads`, `TestEnvelopeRejectsShortAndOversizedFrames`, `TestEnvelopeLargeFrameReusesBuffer` and `TestTrailerError` from `cursor/wire/wire_test.go:460-535` and `devin/wire/wire_test.go:315-390` into `internal/model/connectwire/frame_test.go` as one copy. `TestTrailerError` keeps both trailer cases: `invalid_argument`/`bad input` and `resource_exhausted`/`too large`.
- Spec: none, because behaviour does not change; `docs/decisions/connect-wire.md` records the rule.
- Test: no failing-first test exists, because behaviour does not change. The evidence is the moved tests plus the unchanged `internal/model/cursor` and `internal/model/devin` suites, and `dupl` in `just verify` no longer pairs the two files.
- Decision file: `docs/decisions/connect-wire.md` with `## Invariant` (one implementation of Connect envelope framing, trailer decoding and protowire field helpers), `## Scope` (cursor and devin wire packages) and `## Enforcement` (`internal/model/connectwire/frame_test.go`, the `dupl` linter).
- Anchor: Christopher Alexander.

#### Core-fixes step 10. Pinned writes on Windows (product decision 2)

- New file `internal/tool/patch/pin_root.go`, with no build tag so Linux compiles and tests it:
  - `type rootPin struct { root *os.Root; missing []string }`.
  - `func pinRoot(env tool.Env, display, absolute string) (*rootPin, error)`. It returns `nil, nil` for a non-local runner, as `pin_unix.go:23-26` does, then runs `os.OpenRoot(env.Cwd)` and walks the directory chain.
  - Methods matching `pin_unix.go`:
    - `openChain`: for each component, `Lstat`. Not found records `missing`; a symlink returns `patch: %s contains a symbolic link; patch does not follow links`; otherwise `OpenRoot(component)` and close the previous root.
    - `checkLeaf`: `Lstat` with the same symlink error.
    - `mkdirChain`: nil-safe; `Mkdir` ignoring `fs.ErrExist`, then the `Lstat` symlink check, then `OpenRoot`.
    - `writeFileAt`: `OpenFile(base, os.O_WRONLY|os.O_CREATE|os.O_EXCL, perm.Perm())`, write, `Sync`, then `Chmod(perm)`, then close. Write comes before `Chmod` so a read-only mode cannot block the write on Windows.
    - `readFileAt`: `Lstat`, refusing a symlink, then `ReadFile`.
    - `leafExists` and `leafExistsAfterChain`: the latter clones with `OpenRoot(".")`.
    - `publishOver`: temp write, then `replaceInRoot`.
    - `renameFileAt`: `replaceInRoot`.
    - `linkFileAt`: `Root.Link`.
    - `unlinkFileAt`: `Root.Remove`.
    - `close`: nil-safe.
    - Error texts are the same as `pin_unix.go:110-230`.
- New file `internal/tool/patch/replace_root_other.go` (`//go:build !windows`): `func replaceInRoot(root *os.Root, oldName, newName string) error { return root.Rename(oldName, newName) }`.
- New file `internal/tool/patch/replace_root_windows.go` (`//go:build windows`), mirroring `internal/runner/replace_windows.go:23-139`:
  - Clear a read-only target with `root.Chmod(newName, 0o600)`.
  - Retry `root.Rename` every 10 ms for up to 1 s while the error is `syscall.ERROR_ACCESS_DENIED` or `windows.ERROR_SHARING_VIOLATION`.
  - Restore `0o400` when the rename fails after clearing.
- `internal/tool/patch/pin_other.go`: replace the stubs (`:12-49`) with `type dirPin = rootPin` and `func pinVerifiedComponents(env tool.Env, display, absolute string) (*dirPin, error) { return pinRoot(env, display, absolute) }`. The writer's unpinned fallbacks stay only for remote runners.
- Failing tests:
  - `internal/tool/patch` `TestLocalWritesArePinnedOnEveryPlatform` in a new `pin_platform_test.go`, with no build tag and no skip. Call `pinVerifiedComponents(env, "sub/f.txt", filepath.Join(cwd, "sub", "f.txt"))` on `editTestEnv`, require a non-nil pin, and require `readFileAt("f.txt")` to return `x\n`. It fails on the `windows-latest` CI job before the change (the pin is nil), and passes on Linux both before and after.
  - `internal/tool/patch` `TestRootPinSurvivesParentSwap`, `TestRootPinRefusesSymlinkedMissingParent` and `TestRootPinRefusesSymlinkLeaf` in `pin_root_test.go`. They mirror `pin_test.go:51-147` but call `pinRoot`, `mkdirChain`, `publishOver` and `leafExistsAfterChain` directly, and they skip on Windows only for symlink privileges.
  - The existing `readonly_windows_test.go` `TestReplaceEditsReadOnlyFile` guards the read-only path on Windows CI.
- Spec: `docs/spec/edit-styles.md`, `## Writer` (`:84-92`). Add: "For the local runner, every platform MUST write through a directory handle held from planning. Unix MUST use `openat`-family calls. Other platforms MUST use `os.Root`, so no write resolves outside the workspace. A symbolic link MUST be refused when the pin is taken. On Windows, a read-only target MUST stay read-only after replacement."
- Anchor: Herbert Simon.

#### Core-fixes step 11. Device-code login for headless hosts (product decision 15)

- `internal/app/cli.go`:
  - `loginRequest` (`:192-195`): add `device bool`.
  - `parseLogin` (`:211-229`): add `fs.BoolVar(&request.device, "device-auth", false, "sign in to chatgpt with a device code on a host without a browser")`. The usage condition becomes `fs.NArg() > 0 || (request.apiKey != "" || request.device) && request.provider == ""`, with the usage text `usage: %s login [provider] [--api-key key] [--device-auth]`.
  - `check` (`:232-237`): add `if r.device && r.provider != "chatgpt" { return fmt.Errorf("login %s: --device-auth works only for chatgpt", r.provider) }`. The protocol workstream, Step 12 changes the `apiKeyLoginTargets()` line in this function, so this step adds only the device line below the API-key check.
  - In `localLogin` (`:239-262`), replace the `if request.apiKey != ""` / `else` pair with `switch { case request.apiKey != "": err = store.SetAPIKey(request.provider, request.apiKey); case request.device: err = LoginDevice(ctx, store, request.provider, env.Stdout); default: err = Login(ctx, store, request.provider, env.Stdin, env.Stdout) }`. No helper is added. The function stays under the cyclop limit of 9 (`.golangci.yml:29-30`).
  - `printLoginPrompt` (`:330-336`) replaces the inline `Open %s and enter the code %s to log in to %s.` line with a call to `printDeviceCode` for the user-code case, so the text exists once.
  - Remote login already uses `chatgptDeviceCode` for `chatgpt` (`:308-309`), so the flag needs no remote change.
- `internal/app/login.go`:
  - Add `func LoginDevice(ctx context.Context, store *auth.Store, provider string, out io.Writer) error`. It takes `definition, _ := oauth.ProviderByID(provider)`, runs `oauth.DeviceCode(ctx, definition, func(code, url string) { printDeviceCode(out, provider, url, code) })`, wraps errors as `login %s: %w`, and stores the result with `store.SetCredential(provider, credentialFrom(token))`. It reuses `internal/auth/oauth/device.go:27-36`.
  - Add `func printDeviceCode(out io.Writer, provider, url, code string)`, printing `Open %s and enter the code %s to log in to %s.\n`, the existing literal at `cli.go:332`.
- Failing test: `internal/app` `TestLoginDeviceAuthSignsInWithoutABrowser` in `login_test.go`. It must not call `t.Parallel`, because it swaps `oauth.Providers["chatgpt"]` (`internal/auth/oauth/oauth.go:68`, `var Providers`) and restores it in `t.Cleanup`. The swapped entry copies the `chatgpt` definition and sets `ClientID: "client"`, `DeviceURL`, `DeviceTokenURL` and `TokenURL` to the server, `DeviceRedirectURL: "https://example.test/device-callback"`, and keeps `DeviceVerificationURL: "https://auth.openai.com/codex/device"`, because the test server returns no verification URL and the grant falls back to that field (`internal/auth/oauth/device.go:196`); top-level parallel tests start only after the sequential ones finish.
  - An `httptest` server serves `/usercode` (`{"device_auth_id":"device-id","user_code":"user-code","interval":"0"}`), `/device-token` (404 first, then `{"authorization_code":"code","code_verifier":"verifier"}`) and `/oauth-token` (`{"access_token":"chatgpt-device-access"}`), as in `internal/auth/oauth/device_test.go:133-188`.
  - Run `RunCLI(t.Context(), env, []string{"login", "chatgpt", "--device-auth"})`. Expect exit 0, stdout containing `Open https://auth.openai.com/codex/device and enter the code user-code to log in to chatgpt.` and `logged in to chatgpt`, and an OAuth credential with access `chatgpt-device-access` in `auth.Load(home)`.
  - Before the change the flag is unknown and the command exits with a usage error.
  - Guard test `TestLoginDeviceAuthRejectsOtherProviders`: `login anthropic --device-auth` fails with `login anthropic: --device-auth works only for chatgpt`.
- Spec: `docs/spec/daemon.md:43`, in `## Commands`, after the sentence `Login without a provider MUST list providers.` Add: "`login chatgpt --device-auth` MUST sign in with the ChatGPT device code and print `Open <url> and enter the code <code> to log in to chatgpt.`. With any other provider the flag MUST fail with `login <provider>: --device-auth works only for chatgpt`." In `docs/spec/protocol.md`, under `## Clients`, add the `--device-auth` flag to the bullet that starts with `login [provider]`, and to no other bullet. The protocol workstream, Step 12 owns the account rows and the client rule in that file.
- Lands after: the protocol workstream, Step 12, which changes `apiKeyLoginTargets()` in `check` (`cli.go:233-234`) and the provider tables in `login.go`.
- Anchor: Kenya Hara.

#### Schemas of the core-fixes workstream

- Journal field on each `tool_calls[]` entry of `assistant.message`: `redacted_thinking`.
  - Type: array of string. Optional, and omitted when empty.
  - Each entry: non-empty, opaque, replayed byte for byte. No count or size cap beyond the journal's own.
  - Example: `{"id":"toolu_1","name":"read","freeform":false,"resolved":false,"args":{"path":"a"},"signature":"sig-9","redacted_thinking":["EmwKAhgBEgy3va3pzix"]}`.
- CLI flag `--device-auth` on `dal login` and `dalgona login`.
  - Type: bool, default false. Valid only with provider `chatgpt`.
  - Example: `dal login chatgpt --device-auth`.
- MCP tool name: `<server>__<tool>`, then the rewrite rule from step 6.
  - Pattern: `^[A-Za-z_][A-Za-z0-9_-]{0,63}$`.
  - Example: server `plug:srv`, tool `echo` becomes `plug_srv__echo_<8 hex>`.
- Error literals:
  - `rate limited for <s> s, over the <limit> s wait limit`, where `<limit>` is 60 with no client attached and 300 with one
  - `compaction: the summary is not smaller than the context it replaces (<b> tokens before, <a> after)`
  - `patch: <path> line <n>: "<line>" stands in for omitted code; write the complete text. Nothing was written.`
  - `search: wait for subprocess exit: subprocess did not exit`
  - `web_fetch: wait for subprocess exit: subprocess did not exit`
  - `login <provider>: --device-auth works only for chatgpt`
- New wire notification `dalgona/retry/wait` (step 1a). Params: `threadId` string, `turnId` string, `remainingMs` int64 from 0 up to the remaining wait, in milliseconds. Example: `{"threadId":"t1","turnId":"turn-1","remainingMs":59000}`. It is sent once per second of an attended wait above 60 seconds, and once with `remainingMs` 0 when that wait ends. No journal kind and no config key.

#### Checks of the core-fixes workstream

1. `just verify` passes before each commit (`justfile:121`).
2. Targeted runs:
   - `go test -count=1 ./internal/model/... -run 'TestRunRetries|TestClassify|TestRetryAfterOverLimitStopsTheRequest|TestQuotaRateLimitDoesNotRetry|TestEncodePassesXHighAndMaxEffort|RedactedThinking'`
   - `go test -count=1 ./internal/agent/... -run 'Compaction|RedactedThinking|RetryWait'`
   - `go test -count=1 ./internal/mcp/ -run TestToolNameIsProviderSafe`
   - `go test -count=1 ./internal/tool/patch/ -run 'Omission|Placeholder|Ellipsis|Pin'`
   - `go test -count=1 ./internal/runner/ ./internal/tool/webfetch/ ./internal/tool/search/ -run 'AwaitExit|ExitWait|Kill'`
   - `go test -count=1 ./internal/model/connectwire/ ./internal/model/cursor/... ./internal/model/devin/...`
   - `go test -count=1 ./internal/app/ -run DeviceAuth`
3. Windows without a Windows host:
   - `GOOS=windows GOARCH=amd64 go vet ./internal/tool/patch/`
   - `GOOS=windows GOARCH=amd64 go test -c -o /dev/null ./internal/tool/patch/`
   - `GOOS=windows GOARCH=amd64 go build ./cmd/dal ./cmd/dalgona`
   - The `windows-latest` CI job runs `TestLocalWritesArePinnedOnEveryPlatform` and `TestReplaceEditsReadOnlyFile`.
4. End to end, Retry-After and quota, with no credentials (product decision 16). Start the server in `/tmp` with `MODE=wait` (or `MODE=quota`):
   ```sh
   python3 - <<'EOF' &
   import http.server, os, sys
   mode = os.environ.get('MODE', 'wait')
   class H(http.server.BaseHTTPRequestHandler):
       def do_POST(self):
           self.rfile.read(int(self.headers.get('Content-Length', 0)))
           print('request', flush=True, file=sys.stderr)
           self.send_response(429)
           if mode == 'wait': self.send_header('Retry-After', '400')
           self.send_header('Content-Type', 'application/json'); self.end_headers()
           body = b'{"error":{"type":"rate_limit_error"}}' if mode == 'wait' else b'{"error":{"type":"insufficient_quota","code":"insufficient_quota"}}'
           self.wfile.write(body)
       def log_message(self, *a): pass
   http.server.HTTPServer(('127.0.0.1', 18080), H).serve_forever()
   EOF
   ```
   Then configure `dal` and run one turn:
   ```sh
   export DAL_HOME=/tmp/dh LOCAL_LLM_KEY=x
   mkdir -p $DAL_HOME
   printf 'model = "local/qwen-test"\n[providers.local]\nbase_url = "http://127.0.0.1:18080/v1"\napi_key_env = "LOCAL_LLM_KEY"\n' > $DAL_HOME/config.toml
   go run ./cmd/dal -p hi; echo "exit=$?"
   ```
   - With `MODE=wait` (the server sends `Retry-After: 400`): `dal -p` starts the thread with `thread/start` (`internal/app/cli.go:557`) or resumes it with `thread/resume` (`:561`). Both reach `attach` (`internal/rpc/serve_thread.go:265`, `:395`), which calls `watch` and then `thread.Subscribe()` (`internal/rpc/serve.go:227`), so a client is attached. The output shows `rate limited for 400 s, over the 300 s wait limit` at once, and the server prints `request` once.
   - The 60 s limit with no client has no end-to-end route through `dal -p`, because `thread/start` and `thread/resume` both attach a client (`internal/rpc/serve_thread.go:265`, `:395`). `TestRunRetriesStopsOverTheRetryAfterLimit` covers it.
   - With `MODE=quota`: the server prints `request` exactly once (four times before step 2).
   - `dal -p` exits 1 when the turn status is not `completed` (`internal/app/cli.go:634`, `report`) and prints `dal: turn <status>: <stop>` on stderr.

#### Prerequisites of the core-fixes workstream

- The terminal and web workstreams render `dalgona/retry/wait` (step 1a). They land after this workstream; this workstream ships the notification only. The terminal workstream, Step 11 (live row) and the web workstream, Step 10 (state words and duration formats) render it.
- Shared files with other workstreams:
  - `docs/spec/providers.md:11`: this workstream owns the effort sentence (Step 3); a drift edit to the same line takes Step 3's text.
  - `docs/spec/events.md`: the edit-review workstream, Step 2 (`tool.judged`) adds a row to the same file; merge line by line.
  - `internal/app/cli.go` and `internal/app/login.go`: the protocol workstream, Step 12 changes the `apiKeyLoginTargets()` line in `check` and the provider tables in `login.go`. This workstream's Step 11 also edits `check` and `login.go`, so Step 11 lands after Step 12.
  - `docs/spec/protocol.md`: this workstream adds one methods-table row (Step 1a), one phrase (Step 6) and one flag in the `## Clients` login bullet (Step 11). The protocol workstream owns the rest of the file, including the account rows and the client rule (its Step 12).
- Interfaces needed: the protocol workstream, Step 1 (the `dalgona-schema` directory and `TestDalgonaMessagesHaveSchemas`), and its Step 12 (`check` in `internal/app/cli.go`). Nothing else beyond the current repository at `3c4b6f94`.

