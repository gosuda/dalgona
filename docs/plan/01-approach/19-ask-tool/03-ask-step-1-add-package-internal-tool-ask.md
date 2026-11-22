#### Ask step 1. Maintain `internal/dal/tool/ask`

The package now exists at `internal/dal/tool/ask`.
Update its validation and result boundary under [Ask tool](../../../dal/spec/ask.md) and [Input text](../../../dal/spec/input-text.md).
The outline below describes required behavior and does not prove that the inspected implementation already satisfies it.

Files to update:

- `internal/dal/tool/ask/ask.go`
- `internal/dal/tool/ask/parse.go`
- `internal/dal/tool/ask/clean.go`
- `internal/dal/tool/ask/result.go`
- `internal/dal/tool/ask/ask_test.go`
- `docs/dal/spec/ask.md`
- `docs/dal/decisions/ask-tool.md`

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
	OutcomeTimedOut
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

`func (Tool) Spec() tool.Spec` retains `Name`, `Description`, `Schema`, and `Serial: true`.
The literal texts of `description` and `schema` are in the section "Schemas of the ask workstream".

`parse.go`:

- `func Parse(raw jsontext.Value) ([]Question, error)`.
- It reads with `model.ReadObjectFields`, `model.ReadArrayItems`, `model.ReadString` and `model.ReadBool` (`internal/model/json.go:11,38,60,89`), in the same style as `yield.Parse` (`internal/tool/yield/yield.go:46-68`).
- Split it into `readQuestions`, `readQuestion` and `readOption`, then `validate(questions)`, `validateQuestion(i, q)`, `validateOptions(i, q)`, `validateOption(i, j, o)`, `checkLength(prefix, field, value string, limit int) error`, and `firstRepeat(options []Option, j int) int`. Keep each under cyclomatic complexity 10.
- An unknown member at any level returns `ask: unknown field "<name>"`.
- A JSON shape error returns `ask: arguments do not match the schema: <decoder error>`.
- Use an unexported `type invalid string` error type so `Parse` can tell its own messages from decoder errors.
- Validate strict encoding and reject NUL before accepting any text.
- Preserve the decoded scalar sequence. Do not clean the canonical question, preview, or offered option value.
- Apply required-field nonblank eligibility separately from the accepted source.
- Reject whitespace-only, formatting-only, and terminal-escape-only required fields before publication.
- Validation messages, exact. Indexes are 1-based. Character counts mean Unicode scalars before display projection.
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

`clean.go`, derived question display projection:

- Receive validated source. Never perform replacement decoding or modify the accepted source.
- `0x1b` starts an escape sequence, which is skipped whole (port of `internal/tui/render.go:72-110`):
  - `[` is a CSI and runs to a final byte in `0x40` to `0x7e`.
  - `]`, `P`, `X`, `^` and `_` start a string that runs to BEL or ESC `\\`.
  - Anything else: skip bytes `0x20` to `0x2f`, then one more byte.
- `\n` is kept when `multiline`; otherwise it becomes one space.
- `\t` becomes 4 spaces when `multiline`; otherwise one space.
- Non-layout controls and bidirectional formatting use the safe display policy from the shared input contract.
- A single-line projection may omit surrounding spaces. A preview may omit leading blank lines and trailing spaces.
- Keep a source mapping, preserve offered values, and make distinct options distinguishable after projection.
- Valid required fields remain visible through source-preserving fallback when a glyph is missing.
- These display choices never apply to user answers, their quota checks, or exact option matching.

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

- Validate the complete answer map before producing an accepted reply. Return a field-specific validation error without an outcome on invalid text.
  - Each answer must contain valid encoding, no NUL, and at most 2000 Unicode scalars. Reject the complete invalid answer, never a suffix.
  - Preserve leading and trailing whitespace. Only an empty string has the empty-answer meaning.
  - Deduplicate by exact decoded scalar sequence.
  - A string equal to an original offered label is that option. Other strings are typed text. Keep labels in option order, then typed text.
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
| TimedOut | The exact timed-out result from the ask specification, identifying any recommended fallback | true |
| Failed | `declined: ` + `reply.Detail` | false |

- An answered line is `- <header> (<question>): <answers>`. Answers are joined by `; `.
  - A label is matched against its original value before its safe display projection is printed.
  - On a question with options, a typed text is `Other: ` plus the text quoted with `jsontext.AppendQuote`.
  - On a free-text question, the quoted text alone.
  - An empty list prints `no answer`.

Tests in `internal/dal/tool/ask`, package `ask`, must exercise production parsing, validation, and formatting.
Expected values come from the ask and shared input contracts, not the cleanup implementation.

- `TestParseAcceptsContractShapes`: one single-select, one multi-select with preview, and one free-text question.
- `TestParseRejectsEachLimit`: a table with one case per message above, matched exactly.
- Required-field tests reject empty, Unicode-whitespace-only, formatting-only, and terminal-escape-only values with the field's empty-value error.
- Display tests prove that controls never execute while canonical source and offered values remain unchanged.
- Encoding tests reject malformed UTF-8 and unpaired surrogate escapes while accepting literal U+FFFD.
- Answer tests cover exact option matching, whitespace preservation, duplicate values, existing multiplicity rules, and unknown IDs.
- Boundary tests accept 2000 supplementary scalars and reject 2001 without trimming, truncation, or a partial result.
- `TestFormatTexts`: every outcome row above, plus the answered example in the section "Schemas of the ask workstream".

Docs:

- `docs/dal/spec/ask.md`: tool, arguments, text validation and display, results, and host lifecycle.
- `docs/decisions/ask-tool.md`, with sections `## Invariant`, `## Scope`, `## Enforcement`, `## Rejected alternatives`:
  - Invariant: `ask` is a read-class core tool for top-level threads of both products. It uses `item/tool/requestUserInput`, adds no separate journal event, and keeps the original five-minute deadline.
  - Scope: it permits the new imports `internal/agent` -> `internal/tool/ask`, `internal/rpc` -> `internal/tool/ask` and `internal/script` -> `internal/tool/ask`.
  - Enforcement: the core depguard rule and the tests named in steps 2 to 5.
  - Rejected alternatives: HTML forms (omp), password fields, silent answer truncation, and an extension method instead of the Codex method.

Anchor: Claude Shannon, Tony Hoare.

