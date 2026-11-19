#### Specifications step 11. Correct `docs/spec/events.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/events.md` in one commit. Line numbers are from commit `3c4b6f94`.

Replace these statements. The code is right and the text is stale.

| Line | Now | Write | Evidence |
|---|---|---|---|
| 21 | `created_at` is RFC 3339 UTC | `created_at` is an RFC 3339 timestamp that the encoder writes in UTC | internal/agent/event/encode.go:59-60 `if !e.CreatedAt.IsZero() {` then `fields = append(fields, field("created_at", stringValue(e.CreatedAt.UTC().Format(time.RFC3339Nano))))`; internal/agent/event/codec.go:153 `parsed, err := time.Parse(time.RFC3339Nano, text) |

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 47 | After the state becomes `damaged`, every later fold MUST return an error. | internal/agent/event/state.go:132-133 `if s.Status == StatusDamaged {` then `return errors.New("event: fold: session is damaged")` |
| 59 | The fold MUST set `Preview` to the first 80 Unicode characters of the first user message of kind `prompt` with inline text. Until `Preview` is set, the fold sets `PreviewBlob` to the text blob of the first user message of kind `prompt` that has one. | internal/agent/event/state.go:235-236 `text := []rune(value.Text)` and `s.Preview = string(text[:min(len(text), 80)])`; state.go:239-240 `if value.TextBlob != nil && s.PreviewBlob == nil {`; pinned by internal/agent/even |

Use one word for one thing.

- Keep `thread`. Replace session at lines 21.
- Keep `thread`. Replace session at lines 30.
- Keep `thread`. Replace session at lines 51.
- Keep `thread`. Replace session at lines 58.

Structure:

- Terms: thread is the prose word. Line 21 changes only the words child session to child thread. The wire names session.created, the sessions/ path, and accept_session stay. Evidence for thread: internal/agent/thread.go:35 `type Thread struct {`; events.md lines 21, 40, and 79 already use thread. The Go identifier SessionID at internal/agent/event/state.go:53 is not prose.
- Confirmed without change, with evidence: line 15 blob and spill rules (internal/agent/event/event.go:36 `const MaxInlineBytes = 64 << 10`, event.go:38 `MaxProjectionBytes = MaxInlineBytes + 4<<10`, event.go:381-395, event.go:653-664); line 28 tool.completed row (decode.go:182-197, event.go:360-375); line 77 Tree rules (branch.go:73 `p.nodes = append(p.nodes, TurnNode{TurnID: value.TurnID, ParentTurnID: p.leaf})`, branch.go:81 `p.leaf = p.completed[value.ParentSeq]`, branch.go:90-91); lines 83-92 render rules (context.go:15, 58-60 needsModel, turn.go:150, context.go:91-101 lateInputs, 117-135, 163-168, 171-175, 214-216, 224-237); lines 61-69 fold rules except the script effect item (state.go:244-261, 270-272, 453-456, 478-484, 512-523, 577-597, 351-352 and 468-471 for the deferred grant).

Rewrite these long sentences as written here.

| Line | Write |
|---|---|
| 63 | The fold MUST create the grant for a user `accept_session` decision when the request carries `class`. A request without `class` MUST defer the grant to the `tool.started` of the call. |
| 85 | A `user.message` or `advisor.note` journaled after the `context_seq` of the next `assistant.message` MUST render after that assistant message. The request for that assistant message does not carry the input. |

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 13 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/events.md` prints `style findings: 0`, and `just verify` passes.

