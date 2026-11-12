package agent

import (
	"cmp"
	"context"
	"fmt"
	"maps"
	"net/url"
	"slices"

	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

type pendingQuestion struct {
	prompt  QuestionPrompt
	replies chan asktool.Reply
	opened  uint64
}

func (t *Thread) Ask(ctx context.Context, callID string, questions []asktool.Question) (asktool.Reply, error) {
	if raw, err := url.PathUnescape(callID); err == nil {
		callID = raw
	}
	if ctx.Err() != nil {
		return asktool.Reply{Outcome: asktool.OutcomeInterrupted}, nil
	}
	masked, err := t.host.maskQuestions(questions)
	if err != nil {
		return asktool.Reply{}, err
	}
	prompt := QuestionPrompt{RequestID: newID("req-"), CallID: callID, Questions: masked}
	replies := make(chan asktool.Reply, 1)
	offered, err := ask(t, func(a *actor) bool { return a.offerQuestion(prompt, replies) })
	if err != nil {
		return asktool.Reply{}, err
	}
	if !offered {
		return asktool.Reply{Outcome: asktool.OutcomeNoClient}, nil
	}
	return t.awaitQuestion(ctx, prompt.RequestID, replies), nil
}

func (t *Thread) AnswerQuestion(requestID string, reply asktool.Reply) error {
	return askErr(t, func(a *actor) error {
		if !a.resolveQuestion(requestID, reply) {
			return fmt.Errorf("%w: %s", ErrUnknownRequest, requestID)
		}
		return nil
	})
}

func (h *Host) maskQuestions(questions []asktool.Question) ([]asktool.Question, error) {
	var firstErr error
	mask := func(text string) string {
		masked, err := h.mask(text)
		if err != nil && firstErr == nil {
			firstErr = err
		}
		return masked
	}
	out := make([]asktool.Question, 0, len(questions))
	for _, question := range questions {
		masked := asktool.Question{Header: mask(question.Header), Text: mask(question.Text), MultiSelect: question.MultiSelect}
		for _, option := range question.Options {
			masked.Options = append(masked.Options, asktool.Option{
				Label: mask(option.Label), Description: mask(option.Description), Preview: mask(option.Preview),
			})
		}
		out = append(out, masked)
	}
	return out, firstErr
}

func (a *actor) offerQuestion(prompt QuestionPrompt, replies chan asktool.Reply) bool {
	if len(a.subs) == 0 {
		return false
	}
	a.questionSeq++
	a.questions[prompt.RequestID] = &pendingQuestion{prompt: prompt, replies: replies, opened: a.questionSeq}
	a.publish(Update{Kind: UpdateQuestion, Thread: a.t.id, Question: prompt})
	if len(a.subs) == 0 {
		delete(a.questions, prompt.RequestID)
		return false
	}
	return true
}

func (t *Thread) awaitQuestion(ctx context.Context, requestID string, replies chan asktool.Reply) asktool.Reply {
	select {
	case reply := <-replies:
		return reply
	case <-ctx.Done():
	}
	_, _ = ask(t, func(a *actor) bool {
		return a.resolveQuestion(requestID, asktool.Reply{Outcome: asktool.OutcomeInterrupted})
	})
	select {
	case reply := <-replies:
		return reply
	default:
		return asktool.Reply{Outcome: asktool.OutcomeInterrupted}
	}
}

func (a *actor) resolveQuestion(requestID string, reply asktool.Reply) bool {
	pending, ok := a.questions[requestID]
	if !ok {
		return false
	}
	delete(a.questions, requestID)
	pending.replies <- reply
	prompt := pending.prompt
	prompt.Resolved = true
	prompt.Resolution = reply.Outcome
	a.publish(Update{Kind: UpdateQuestion, Thread: a.t.id, Question: prompt})
	return true
}

func (a *actor) declineQuestions() {
	for requestID := range a.questions {
		a.resolveQuestion(requestID, asktool.Reply{Outcome: asktool.OutcomeNoClient})
	}
}

func (a *actor) subscribe(ch chan Update) {
	a.subs[ch] = struct{}{}
	open := slices.SortedFunc(maps.Values(a.questions), func(x, y *pendingQuestion) int {
		return cmp.Compare(x.opened, y.opened)
	})
	for _, pending := range open {
		a.sendUpdate(ch, Update{Kind: UpdateQuestion, Thread: a.t.id, Question: pending.prompt})
	}
}

func (a *actor) sendUpdate(ch chan Update, update Update) {
	select {
	case ch <- update:
	default:
		close(ch)
		delete(a.subs, ch)
		a.t.host.log.Warn("agent: closing slow subscriber", "thread", a.t.id)
	}
}
