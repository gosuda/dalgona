package battery

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/ratchet"
	"github.com/gosuda/dalgona/internal/tool"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

func ratchetTools(ctx context.Context) []tool.Tool {
	thread, ok := agent.ThreadFromContext(ctx)
	if !ok || !thread.TopLevel() {
		return nil
	}
	return []tool.Tool{ratchet.New(threadAsker{thread: thread})}
}

type threadAsker struct {
	thread *agent.Thread
}

func (a threadAsker) Ask(ctx context.Context, callID string, q ratchet.Question) (ratchet.Answer, error) {
	question, err := askQuestion(q)
	if err != nil {
		return ratchet.Answer{}, err
	}
	reply, err := a.thread.Ask(ctx, callID, []asktool.Question{question})
	if err != nil {
		return ratchet.Answer{}, err
	}
	return answerFrom(q, reply)
}

func askQuestion(q ratchet.Question) (asktool.Question, error) {
	if q.Text == "" {
		return asktool.Question{}, errors.New("ratchet approval: question text is empty")
	}
	if n := utf8.RuneCountInString(q.Text); n > asktool.MaxQuestion {
		return asktool.Question{}, fmt.Errorf("ratchet approval: question is longer than %d characters", asktool.MaxQuestion)
	}
	options := make([]asktool.Option, 0, len(q.Options))
	for _, o := range q.Options {
		if utf8.RuneCountInString(o.Preview) > asktool.MaxPreview || strings.Count(o.Preview, "\n")+1 > asktool.MaxPreviewLines {
			return asktool.Question{}, fmt.Errorf("ratchet approval: preview is longer than %d characters or %d lines", asktool.MaxPreview, asktool.MaxPreviewLines)
		}
		options = append(options, asktool.Option{Label: o.Label, Description: o.Description, Preview: o.Preview})
	}
	return asktool.Question{Header: q.Header, Text: q.Text, Options: options}, nil
}

func answerFrom(q ratchet.Question, reply asktool.Reply) (ratchet.Answer, error) {
	switch reply.Outcome {
	case asktool.OutcomeAnswered:
		return answerOf(q, reply.Answers), nil
	case asktool.OutcomeNoClient:
		return ratchet.Answer{}, ratchet.ErrNoClient
	case asktool.OutcomeCancelled, asktool.OutcomeInterrupted:
		return ratchet.Answer{}, ratchet.ErrCancelled
	case asktool.OutcomeFailed:
		return ratchet.Answer{}, fmt.Errorf("ratchet approval failed: %s", reply.Detail)
	}
	return ratchet.Answer{}, errors.New("ratchet approval: unknown reply outcome")
}

func answerOf(q ratchet.Question, answers [][]string) ratchet.Answer {
	text := cmp.Or(slices.Concat(answers...)...)
	for _, o := range q.Options {
		if o.Label == text {
			return ratchet.Answer{Label: text}
		}
	}
	return ratchet.Answer{Other: text}
}
