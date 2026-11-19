#### Live-debug step 1. Add `internal/llmstub`, the scripted chat-completions handler

Files to add:

- `internal/llmstub/script.go`
- `internal/llmstub/server.go`
- `internal/llmstub/sse.go`
- `internal/llmstub/llmstub_test.go`
- `internal/model/chatcompat/llmstub_test.go`
- `docs/spec/live-debug.md`, a new topic. This step writes the sections "Scripted server", "Script format" and "SSE framing".
- `docs/decisions/llmstub-test-support.md`. It records the new import edge `internal/model/chatcompat` (tests only) to `internal/llmstub`, with a "Rejected alternatives" section: rival 1 is the exported chatcompat constructor, rival 2 is the duplicated fakes. Copy the section layout of `docs/decisions/automated-approval.md`.

Symbols:

- `type Script struct { Models []Model; Turns []Turn }`
- `type Model struct { ID string; ContextLength int; InputModalities []string }`. `InputModalities` is written as `architecture.input_modalities` and is omitted when empty.
- `type Turn struct { ID string; When When; Once bool; Status int; RetryAfter string; Body jsontext.Value; Steps []Step }`
- `type When struct { User, ToolResult, AnyText string }`
- `type Step struct { Text, Reasoning string; ToolCall *ToolCall; DelayMS int; Finish string; Usage *Usage; ErrorFrame *ErrorFrame; Drop bool }`
- `type ToolCall struct { ID, Name string; Arguments jsontext.Value; Chunks int }`
- `type Usage struct { PromptTokens, CompletionTokens int }`
- `type ErrorFrame struct { Code, Message string }`
- `func Parse(name string, raw []byte) (Script, error)` decodes with `jsontext.Decoder` and rejects unknown keys. It then calls `validateTurn(i int, t Turn) error` and `validateStep(turn, i int, s Step, last bool) error`, each at cyclomatic complexity 9 or less. A turn error has the form `llmstub: <name>: turn <n>: <reason>`. A step error has the form `llmstub: <name>: turn <n>: step <m>: <reason>`.
- `type Options struct { Key string; Record io.Writer; Log io.Writer }`
- `type handler struct { s Script; o Options; mu sync.Mutex; spent map[string]bool; seq int }`.
- `func NewHandler(s Script, o Options) http.Handler` serves exactly `POST /v1/chat/completions` and `GET /v1/models`. Any other method or path gets 404. A missing or wrong `Authorization: Bearer <Key>` gets 401 with body `{"error":{"code":"llmstub_auth","message":"bad bearer token"}}`.
- `type lastMessage struct{ Role, Text, ToolCallID string }` and `type requestView struct{ Last lastMessage; Texts []string }`, where `Texts` holds the text of every message. `func (h *handler) pick(v requestView) (Turn, bool)` returns the first turn in file order that matches and is not spent. It holds `h.mu`, marks a matched `once` turn spent, and its complexity stays under 8.
- `func decodeRequest(r io.Reader) (requestView, error)` reads `messages[]` with `role`, `content` (a string or an array of `{type:"text",text}` parts, concatenated) and `tool_call_id`. It skips every other key with `SkipValue`.
- The handler's serve path loops over the turn's steps and calls `writeStep(ctx context.Context, w http.ResponseWriter, rc *http.ResponseController, s Step) error` for each. `writeStep` is a switch with one case per step kind (7 cases). There is no separate turn writer.
- `sse.go` holds `frame(w, rc *http.ResponseController, v jsontext.Value) error`, which writes `data: ` + v + `\n\n` and then calls `rc.Flush()`. It also holds the encoders `textFrame`, `reasoningFrame`, `toolFrames` (it splits `Arguments` into `Chunks` parts at UTF-8 rune boundaries, as equal as the boundaries allow), `finishFrame`, `errorFrame` and `doneFrame`.
- A delay waits on `select { case <-time.After(d): case <-ctx.Done(): return ctx.Err() }`, so an interrupt closes promptly.
- Recording: before replying, the handler appends one line `{"seq":<n>,"path":"/v1/chat/completions","turn":"<id or unmatched>","body":<request body verbatim>}`. The log line is `llmstub: <seq> <turn id|unmatched> <path>`.
- Unmatched requests get status 400 with body `{"error":{"code":"llmstub_unmatched","message":"no scripted turn matches: <last message text, first 80 runes>"}}`.

Tests that fail before the change (they do not compile, because the package is absent):

- `internal/llmstub`:
  - `TestParseRejectsStepWithTwoKinds`
  - `TestParseRejectsStatusTurnWithSteps`
  - `TestParseRejectsRetryAfterWithoutRetryableStatus`
  - `TestParseRejectsTurnWithoutTerminalStep`
  - `TestUnmatchedRequestIsTerminal400` (status 400 and code `llmstub_unmatched`)
  - `TestWrongBearerIs401`
  - `TestOnceTurnIsSpent` (a 429 turn then a success turn with the same `when`: the first request gets 429 and the second gets 200)
  - `TestDelayEndsOnClientCancel` (a 60 s delay: after cancelling the request context, the handler returns within 1 s)
  - `TestModelsListShape` (body `{"object":"list","data":[{"id":"qwen-test","context_length":32000,"architecture":{"input_modalities":["text","image"]}}]}`; the oracle is `docs/spec/providers.md` "Chat-compatible lists MUST read `context_length`")
- `internal/model/chatcompat`:
  - `TestLLMStubTurnDecodes`: a script with reasoning, text, two tool calls (one split in 3 chunks) and finish `tool_calls` with usage 5/3. The test asserts, in order, `ReasoningDelta`, `TextDelta`, two `ToolCall` values with the exact argument bytes, `Usage{Input:5,Output:3}` and `Done{StopReason:"toolUse"}`. The oracle is `decode.go:121-169`.
  - `TestLLMStubRetryAfterReplacesDelay`: a `once` 429 turn with `retry_after` `"2"`, then a success turn. `fastRetry` gets a `Sleep` that records the delay. The test asserts that exactly 2 s was recorded (oracle: `docs/spec/providers.md` "A valid Retry-After value up to 60 seconds MUST replace the delay").
  - `TestLLMStubErrorFrameFailsWithoutRetry`: text then `error_frame`. The test asserts a stream failure and zero retries (providers spec: a stream that emitted text MUST NOT retry).

Anchor: Claude Shannon, Tony Hoare.

