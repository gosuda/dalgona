package advisor

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"html"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/rules"
	"github.com/gosuda/dalgona/internal/tool"
)

const (
	adviseName       = "advise"
	verdictDiscarded = "discarded"
)

var destructiveRules = []string{"recursive-delete", "git-destructive", "publish", "deploy"}

type advice struct {
	note     string
	severity Severity
}

type adviseTool struct{ a *advisor }

func (adviseTool) Spec() tool.Spec {
	return tool.Spec{
		Name:        adviseName,
		Description: AdviseDescription,
		Schema:      jsontext.Value(`{"type":"object","properties":{"note":{"type":"string"},"severity":{"type":"string","enum":["nit","concern","blocker"]}},"required":["note","severity"],"additionalProperties":false}`),
		Keep:        tool.KeepHead,
	}
}

func (adviseTool) Class() tool.Class { return tool.ClassRead }

func (t adviseTool) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	if len(call.Args) > event.MaxInlineBytes {
		return tool.Result{}, errors.New("advise: note is too long")
	}
	parsed, err := parseAdvice(call.Args)
	if err != nil {
		return tool.Result{}, err
	}
	verdict, err := t.a.admit(ctx, parsed)
	if err != nil {
		return tool.Result{}, err
	}
	return tool.Result{Output: []byte(verdict), OK: true}, nil
}

func parseAdvice(raw jsontext.Value) (advice, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return advice{}, fmt.Errorf("advise: read arguments: %w", err)
	}
	if start.Kind() != '{' {
		return advice{}, errors.New("advise: arguments must be an object")
	}
	var note, severity string
	for decoder.PeekKind() != '}' {
		name, value, err := readMember(decoder)
		if err != nil {
			return advice{}, err
		}
		switch name {
		case "note":
			note = value
		case "severity":
			severity = value
		}
	}
	parsed, ok := ParseSeverity(severity)
	if !ok {
		return advice{}, errors.New("advise: severity must be one of nit, concern, blocker")
	}
	return advice{note: note, severity: parsed}, nil
}

func readMember(decoder *jsontext.Decoder) (string, string, error) {
	token, err := decoder.ReadToken()
	if err != nil {
		return "", "", fmt.Errorf("advise: read argument name: %w", err)
	}
	name := token.String()
	if decoder.PeekKind() != '"' {
		if skipErr := decoder.SkipValue(); skipErr != nil {
			return "", "", fmt.Errorf("advise: read argument %s: %w", name, skipErr)
		}
		return name, "", nil
	}
	value, err := decoder.ReadToken()
	if err != nil {
		return "", "", fmt.Errorf("advise: read argument %s: %w", name, err)
	}
	return name, value.String(), nil
}

type pendingNote struct {
	key  string
	text string
	advice
}

func pendingNotes(events []event.Seqd, maxNotes int) []pendingNote {
	calls := make(map[string]advice)
	replay := NewGuard(maxNotes)
	var pending []pendingNote
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.AssistantMessage:
			collectAdvice(calls, value.ToolCalls)
		case event.ToolCompleted:
			parsed, ok := calls[value.CallID]
			if ok && value.Output == string(VerdictAccepted) {
				pending = queueNote(pending, replay.Admit(parsed.note, parsed.severity), parsed)
			}
		}
	}
	return pending
}

func collectAdvice(calls map[string]advice, toolCalls []event.ToolCall) {
	for _, call := range toolCalls {
		if call.Name != adviseName || call.ArgsBlob != nil {
			continue
		}
		if parsed, err := parseAdvice(call.Args); err == nil {
			calls[call.ID] = parsed
		}
	}
}

func queueNote(pending []pendingNote, admission Admission, parsed advice) []pendingNote {
	if admission.Verdict != VerdictAccepted {
		return pending
	}
	if admission.Displaced != "" {
		pending = slices.DeleteFunc(pending, func(p pendingNote) bool { return p.key == admission.Displaced })
	}
	if parsed.severity == SeverityBlocker {
		return pending
	}
	return append(pending, pendingNote{key: admission.Key, text: parsed.note, advice: parsed})
}

func unsafeOutput(events []event.Seqd, input string) bool {
	plain := html.UnescapeString(input)
	for _, item := range events {
		message, ok := item.Event.(event.AssistantMessage)
		if !ok {
			continue
		}
		if message.TextBlob != nil || destructive(message.Text, plain) {
			return true
		}
	}
	return false
}

func destructive(text, input string) bool {
	for line := range strings.Lines(text) {
		for _, candidate := range directives(line) {
			name, part, matched := rules.Check(candidate)
			if matched && slices.Contains(destructiveRules, name) && !strings.Contains(input, part) {
				return true
			}
		}
	}
	return false
}

func directives(line string) []string {
	spans := strings.Split(line, "`")
	out := make([]string, 0, 1+len(spans)/2)
	bare := strings.TrimSpace(strings.Join(spans, ""))
	out = append(out, strings.TrimSpace(strings.TrimPrefix(bare, "$")))
	for i := 1; i < len(spans); i += 2 {
		out = append(out, strings.TrimSpace(spans[i]))
	}
	return out
}

var _ tool.Tool = adviseTool{}
