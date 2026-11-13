package ask

import (
	"context"
	"encoding/json/jsontext"
	"strconv"

	"github.com/gosuda/dalgona/internal/tool"
)

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

const description = "Ask the user 1 to 4 questions and wait for the answers. " +
	"Use it only when the choice belongs to the user and the code, the docs and the conversation cannot settle it: " +
	"a preference, a trade-off, or a missing requirement. " +
	"Do not use it to ask permission to run a tool or to report progress.\n\n" +
	"Each question has a `header` (a tab label of at most 30 characters) and a `question` (one sentence). " +
	"Give 2 to 6 `options`, each with a `label` of a few words and a `description` of what that choice means. " +
	"Put the option you recommend first; the user sees it marked as recommended. " +
	"Add `preview` (plain text, at most 40 lines, shown in monospace) when the user must compare concrete output such as code or a layout. " +
	"Set `multi_select` to true when several options can apply. " +
	"Omit `options` for a free-text question.\n\n" +
	"Every question with options gets an Other row where the user can type an answer, so never add an option named Other. " +
	"The result lists one line per question; a typed answer appears in quotes after `Other:`. " +
	"When the user skips, dismisses, or cannot be reached, the result says so; " +
	"continue with your best judgment and state the assumptions you made."

const schema = `{"type":"object","properties":{"questions":{"type":"array","minItems":1,"maxItems":4,"items":{"type":"object","properties":{"header":{"type":"string","maxLength":30,"description":"Tab label of a few words."},"question":{"type":"string","maxLength":500,"description":"The question as one sentence."},"options":{"type":"array","minItems":2,"maxItems":6,"description":"2 to 6 choices, recommended first. Omit for a free-text question.","items":{"type":"object","properties":{"label":{"type":"string","maxLength":80},"description":{"type":"string","maxLength":300},"preview":{"type":"string","maxLength":2000}},"required":["label","description"],"additionalProperties":false}},"multi_select":{"type":"boolean","description":"Let the user pick several options."}},"required":["header","question"],"additionalProperties":false}}},"required":["questions"],"additionalProperties":false}`

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
	NoOther     bool
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

func QuestionID(index int) string { return "q" + strconv.Itoa(index+1) }

func (Tool) Spec() tool.Spec {
	return tool.Spec{Name: Name, Description: description, Schema: jsontext.Value(schema)}
}

func (Tool) Class() tool.Class { return tool.ClassRead }

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

func (q Question) hasLabel(text string) bool {
	for _, option := range q.Options {
		if option.Label == text {
			return true
		}
	}
	return false
}
