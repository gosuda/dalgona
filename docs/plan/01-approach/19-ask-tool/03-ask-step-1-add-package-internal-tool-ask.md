#### Ask step 1. Add package `internal/tool/ask`

Files to add:

- `internal/tool/ask/ask.go`
- `internal/tool/ask/parse.go`
- `internal/tool/ask/clean.go`
- `internal/tool/ask/result.go`
- `internal/tool/ask/ask_test.go`
- `docs/spec/ask.md`
- `docs/decisions/ask-tool.md`

`ask.go`:

```go
package ask

const Name = "ask"

const (
	MaxQuestions    = 4
	MinOptions      = 2
	MaxOptions      = 6
	MaxHeader       = 30
	MaxQuestion     = 500
	MaxLabel        = 80
	MaxDescription  = 300
	MaxPreview      = 2000
	MaxPreviewLines = 40
	MaxAnswer       = 2000
)

type Option struct {
	Label       string
	Description string
	Preview     string
}

type Question struct {
	Header      string
	Text        string
	Options     []Option
	MultiSelect bool
}

type Outcome int

const (
	OutcomeAnswered Outcome = iota + 1
	OutcomeCancelled
	OutcomeNoClient
	OutcomeInterrupted
	OutcomeFailed
)

type Reply struct {
	Outcome Outcome
	Answers [][]string
	Detail  string
}

type Asker interface {
	Ask(ctx context.Context, callID string, questions []Question) (Reply, error)
}

type Tool struct{ asker Asker }

func New(asker Asker) Tool { return Tool{asker: asker} }
func (Tool) Class() tool.Class { return tool.ClassRead }
func QuestionID(index int) string { return "q" + strconv.Itoa(index+1) }

func (t Tool) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	questions, err := Parse(call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	reply, err := t.asker.Ask(ctx, call.ID, questions)
	if err != nil {
		return tool.Result{}, err
	}
	return Format(questions, reply), nil
}
```

`func (Tool) Spec() tool.Spec` in `ask.go` returns `tool.Spec{Name: Name, Description: description, Schema: jsontext.Value(schema)}`. The literal texts of `description` and `schema` are in the section "Schemas of the ask workstream".

`parse.go`:

- `func Parse(raw jsontext.Value) ([]Question, error)`.
- It reads with `model.ReadObjectFields`, `model.ReadArrayItems`, `model.ReadString` and `model.ReadBool` (`internal/model/json.go:11,38,60,89`), in the same style as `yield.Parse` (`internal/tool/yield/yield.go:46-68`).
- Split it into `readQuestions`, `readQuestion` and `readOption`, then `validate(questions)`, `validateQuestion(i, q)`, `validateOptions(i, q)`, `validateOption(i, j, o)`, `checkLength(prefix, field, value string, limit int) error`, and `firstRepeat(options []Option, j int) int`. Keep each under cyclomatic complexity 10.
- An unknown member at any level returns `ask: unknown field "<name>"`.
- A JSON shape error returns `ask: arguments do not match the schema: <decoder error>`.
- Use an unexported `type invalid string` error type so `Parse` can tell its own messages from decoder errors.
- Clean each text while reading:
  - header, question, label and description: `Clean(s, false)`.
  - preview: `Clean(s, true)`.
- Validation messages, exact. Indexes are 1-based; "characters" means runes after cleaning.
  - `ask: questions needs 1 to 4 items, got <n>`
  - `ask: question <i>: header is empty`
  - `ask: question <i>: header is longer than 30 characters`
  - `ask: question <i>: question is empty`
  - `ask: question <i>: question is longer than 500 characters`
  - `ask: question <i>: options needs 2 to 6 items, or none for a free-text question, got <n>`
  - `ask: question <i>: multi_select needs options`
  - `ask: question <i> option <j>: label is empty`
  - `ask: question <i> option <j>: label is longer than 80 characters`
  - `ask: question <i> option <j>: description is empty`
  - `ask: question <i> option <j>: description is longer than 300 characters`
  - `ask: question <i> option <j>: preview is longer than 2000 characters or 40 lines`
  - `ask: question <i> option <j>: label "<label>" repeats option <k>` (compared with `strings.EqualFold`)
  - `ask: question <i> option <j>: the user always gets an Other row; remove this option` (label equal to `other` in any case)
  - `ask: question <i> repeats question <k>` (question text compared with `strings.EqualFold`)

`clean.go`, `func Clean(text string, multiline bool) string`:

- Run `strings.ToValidUTF8(text, "\uFFFD")` first.
- `0x1b` starts an escape sequence, which is skipped whole (port of `internal/tui/render.go:72-110`):
  - `[` is a CSI and runs to a final byte in `0x40` to `0x7e`.
  - `]`, `P`, `X`, `^` and `_` start a string that runs to BEL or ESC `\\`.
  - Anything else: skip bytes `0x20` to `0x2f`, then one more byte.
- `\n` is kept when `multiline`; otherwise it becomes one space.
- `\t` becomes 4 spaces when `multiline`; otherwise one space.
- Every other rune for which `unicode.IsControl` holds is dropped (C0, DEL, C1).
- U+202A to U+202E and U+2066 to U+2069 are dropped.
- Single-line results get `strings.TrimSpace`. Multiline results drop leading blank lines and trailing whitespace.
- Functions: `Clean`, `skipEscape`, `skipCSI`, `skipString`, `skipShort`, and `keep(r rune, multiline bool) (string, bool)`.

`result.go`:

- Text constants, exact:

```go
const (
	TextNoClient    = "declined: no client could answer this question"
	TextSkipped     = "The user skipped the questions and has no preference. Choose for yourself and say which choice you made."
	TextCancelled   = "The user dismissed the questions without answering. Continue with your best judgment, or stop and say what you need if you cannot continue."
	TextInterrupted = "interrupted: the user interrupted the turn before answering"
	TextRestarted   = "interrupted: the host restarted before the user answered"
	answersHead     = "User answers:"
)
```

- `func Answers(questions []Question, byID map[string][]string) [][]string`. For each question index `i`, take `byID[QuestionID(i)]`:
  - `Clean(s, false)` each string, drop empty ones, cut each to `MaxAnswer` runes, and drop repeats.
  - A string equal to a label of the question (exact match after cleaning) is that label; any other string is typed text. Order: labels in option order first, then typed texts in arrival order.
  - Keep at most 1 string for a single-select or free-text question, and at most `len(Options)+1` for multi-select.
  - Ids not in the request are ignored.
- `func Format(questions []Question, reply Reply) tool.Result`:

| Outcome | Output | OK |
|---|---|---|
| Answered, every list empty | `TextSkipped` | true |
| Answered | `User answers:` then one line per question | true |
| Cancelled | `TextCancelled` | true |
| NoClient | `TextNoClient` | false |
| Interrupted | `TextInterrupted` | false |
| Failed | `declined: ` + `reply.Detail` | false |

- An answered line is `- <header> (<question>): <answers>`. Answers are joined by `; `.
  - A label is printed as is.
  - On a question with options, a typed text is `Other: ` plus the text quoted with `jsontext.AppendQuote`.
  - On a free-text question, the quoted text alone.
  - An empty list prints `no answer`.

Tests in `internal/tool/ask/ask_test.go`, package `ask`. All fail before this step because the package does not exist; each expected value comes from `docs/spec/ask.md`.

- `TestParseAcceptsContractShapes`: one single-select, one multi-select with preview, and one free-text question.
- `TestParseRejectsEachLimit`: a table with one case per message above, matched exactly.
- `TestCleanStripsTerminalControls`: inputs such as `"\x1b[31mDB\x1b[0m"`, `"\x1b]0;title\x07x"`, `"a\u202Eb"`, `"\x9bq"` and `"a\nb"` single-line give `DB`, `x`, `ab`, `q` and `a b`.
- `TestAnswersNormalizes`: label order, cap, repeats, the single-select first-only rule, and unknown ids.
- `TestFormatTexts`: every outcome row above, plus the answered example in the section "Schemas of the ask workstream".

Docs:

- `docs/spec/ask.md`: sections `## Tool`, `## Arguments`, `## Cleaning`, `## Results`, holding the schema, the limits, the messages and the strings above.
- `docs/decisions/ask-tool.md`, with sections `## Invariant`, `## Scope`, `## Enforcement`, `## Rejected alternatives`:
  - Invariant: `ask` is a read-class core tool for top-level threads of both products. It reaches a person only through `item/tool/requestUserInput`. It adds no journal event. It has no timeout.
  - Scope: it permits the new imports `internal/agent` -> `internal/tool/ask`, `internal/rpc` -> `internal/tool/ask` and `internal/script` -> `internal/tool/ask`.
  - Enforcement: the core depguard rule and the tests named in steps 2 to 5.
  - Rejected alternatives: HTML forms (omp), password fields, a timeout, and an extension method instead of the Codex method.

Anchor: Claude Shannon, Tony Hoare.

