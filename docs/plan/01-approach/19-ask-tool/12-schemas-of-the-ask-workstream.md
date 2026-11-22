#### Schemas of the ask workstream

No config key and no journal event are added.

##### Tool argument schema, literal `schema` in `internal/tool/ask/ask.go`

```json
{"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"header":{"type":"string","maxLength":30,"description":"Tab label of a few words."},"question":{"type":"string","maxLength":500,"description":"The question as one sentence."},"options":{"type":"array","minItems":2,"maxItems":6,"description":"2 to 6 choices, recommended first. Omit for a free-text question.","items":{"type":"object","properties":{"label":{"type":"string","maxLength":80},"description":{"type":"string","maxLength":300},"preview":{"type":"string","maxLength":2000}},"required":["label","description"],"additionalProperties":false}},"multi_select":{"type":"boolean","description":"Let the user pick several options."}},"required":["header","question"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false}
```

Tool argument fields are snake_case like other tool arguments: `multi_select`. Example:

```json
{"questions":[{"header":"Database","question":"Which database should the service use?","options":[{"label":"SQLite","description":"One file, no server to run."},{"label":"Postgres","description":"A server with replication.","preview":"services:\n  db:\n    image: postgres:17"}]},{"header":"Features","question":"Which features ship first?","multi_select":true,"options":[{"label":"Search","description":"Full-text search."},{"label":"Export","description":"CSV export."},{"label":"Sync","description":"Two-way sync."}]},{"header":"Name","question":"What should the service be called?"}]}
```

Answered output for answers `[["Postgres"], ["Search","Export"], ["ledger-svc"]]`:

```
User answers:
- Database (Which database should the service use?): Postgres
- Features (Which features ship first?): Search; Export
- Name (What should the service be called?): "ledger-svc"
```

The same output with `[["Mongo"], [], []]`:

```
User answers:
- Database (Which database should the service use?): Other: "Mongo"
- Features (Which features ship first?): no answer
- Name (What should the service be called?): no answer
```

##### Tool description, literal `description`

```
Ask the user 1 to 4 questions and wait for the answers. Use it only when the choice belongs to the user and the code, the docs and the conversation cannot settle it: a preference, a trade-off, or a missing requirement. Do not use it to ask permission to run a tool or to report progress.

Each question has a `header` (a tab label of at most 30 characters) and a `question` (one sentence). Give 2 to 6 `options`, each with a `label` of a few words and a `description` of what that choice means. Put the option you recommend first; the user sees it marked as recommended. Add `preview` (plain text, at most 40 lines, shown in monospace) when the user must compare concrete output such as code or a layout. Set `multi_select` to true when several options can apply. Omit `options` for a free-text question.

Every question with options gets an Other row where the user can type an answer, so never add an option named Other. The result lists one line per question; a typed answer appears in quotes after `Other:`. When the user skips, dismisses, or cannot be reached, the result says so; continue with your best judgment and state the assumptions you made.

If the user does not answer within 5 minutes, the result says so and uses the first option of each question that has options, so make the first option one that is safe to use without an answer.
```

##### Wire request `item/tool/requestUserInput`

The server sends this request. The JSON-RPC `id` is the request id. Fields are camelCase.

| Field | Type | Rule |
|---|---|---|
| `threadId` | string | required |
| `turnId` | string | required, the current turn |
| `itemId` | string | required, the `ask` call id (the `dynamicToolCall` item id, `internal/rpc/serve_items.go:20`), or the `exec` call id for the sandbox choice |
| `isBlocking` | bool | always `true` |
| `questions[].id` | string | `q1` to `q4` for an `ask` question; `sandbox` for the sandbox choice (the protocol workstream, Step 13) |
| `questions[].header` | string | exact validated source, nonblank, at most 30 scalars |
| `questions[].question` | string | exact validated source, nonblank, at most 500 scalars |
| `questions[].isOther` | bool | `true` for an `ask` question with options; `false` for the sandbox choice |
| `questions[].isSecret` | bool | always `false` |
| `questions[].multiSelect` | bool | extension, always written |
| `questions[].options` | array or null | null for free text |
| `options[].label` | string | exact validated source, nonblank, at most 80 scalars |
| `options[].description` | string | exact validated source, nonblank, at most 300 scalars |
| `options[].preview` | string | extension, omitted when empty |

`autoResolutionMs` is never sent; it is deprecated in the schema.

```json
{"jsonrpc":"2.0","id":"req-k3v9x2m4q8w1z7c5","method":"item/tool/requestUserInput","params":{"threadId":"t-1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[{"id":"q1","header":"Database","question":"Which database should the service use?","isOther":true,"isSecret":false,"multiSelect":false,"options":[{"label":"SQLite","description":"One file, no server to run."},{"label":"Postgres","description":"A server with replication.","preview":"services:\n  db:\n    image: postgres:17"}]},{"id":"q2","header":"Name","question":"What should the service be called?","isOther":false,"isSecret":false,"multiSelect":false,"options":null}]}}
```

The two extensions are `multiSelect` and `preview`. A client that ignores them still works: it sends one answer per question (valid for multi-select) and shows no preview. `ToolRequestUserInputParams.json` and `ToolRequestUserInputOption` have no `additionalProperties: false`, so the extended request still validates.

##### Wire response

The vendored shape `{answers: {<id>: {answers: [string]}}}`:

```json
{"jsonrpc":"2.0","id":"req-k3v9x2m4q8w1z7c5","result":{"answers":{"q1":{"answers":["Postgres"]},"q2":{"answers":["ledger-svc"]}}}}
```

A dismiss:

```json
{"jsonrpc":"2.0","id":"req-k3v9x2m4q8w1z7c5","result":{"answers":{}}}
```

The client answer to a question, under the Codex schema `ToolRequestUserInputResponse`:
- `answers`: object map from question id to `{ answers: string[] }`. Each value is an exact offered label or accepted typed text. Multi-select permits the option count plus one typed answer, with an empty list for a skipped question.
- Cancel sends `{"answers": {}}`.
- Example: `{"answers":{"q1":{"answers":["Keep the cache"]},"q2":{"answers":["src/a.go","src/b.go"]}}}`.

##### `serverRequest/resolved`, one additive member

```json
{"jsonrpc":"2.0","method":"serverRequest/resolved","params":{"threadId":"t-1","requestId":"req-k3v9x2m4q8w1z7c5","resolution":"answered"}}
```

`resolution` is one of `answered`, `cancelled`, `interrupted`, `timedOut`, or `declined`. It is present only for questions.

##### Correction warning

```json
{"jsonrpc":"2.0","method":"warning","params":{"threadId":"t-1","requestId":"req-k3v9x2m4q8w1z7c5","field":"answers.q1.answers[0]","reason":"text_too_large","message":"invalid text: answers.q1.answers[0]: text_too_large"}}
```

`requestId`, `field`, and `reason` supplement the existing warning contract.
For correction, `requestId`, `field`, and `reason` are present. Other warnings may omit them.
The warning leaves the original request pending and sends no resolution.
The corrected response uses the same ID and nested answer shape before the original deadline.
Validate the warning extension against both the standalone and server-notification vendored schemas.

##### Thread status

`{"type":"active","activeFlags":["waitingOnUserInput"]}`, a vendored enum value.

##### Go types in `internal/rpc/types.go`

```go
ToolRequestUserInputParams struct {
	ThreadID   string                         `json:"threadId"`
	TurnID     string                         `json:"turnId"`
	ItemID     string                         `json:"itemId"`
	IsBlocking bool                           `json:"isBlocking"`
	Questions  []ToolRequestUserInputQuestion `json:"questions"`
}
ToolRequestUserInputQuestion struct {
	ID          string                       `json:"id"`
	Header      string                       `json:"header"`
	Question    string                       `json:"question"`
	IsOther     bool                         `json:"isOther"`
	IsSecret    bool                         `json:"isSecret"`
	MultiSelect bool                         `json:"multiSelect"`
	Options     []ToolRequestUserInputOption `json:"options"`
}
ToolRequestUserInputOption struct {
	Label       string `json:"label"`
	Description string `json:"description"`
	Preview     string `json:"preview,omitzero"`
}
ToolRequestUserInputResponse struct {
	Answers map[string]ToolRequestUserInputAnswer `json:"answers"`
}
ToolRequestUserInputAnswer struct {
	Answers []string `json:"answers"`
}
```

