package agent

import (
	"cmp"
	"context"
	"fmt"
	"maps"
	"net/url"
	"slices"
	"time"

	asktool "github.com/gosuda/dalgona/internal/dal/tool/ask"
)

type pendingQuestion struct {
	prompt  QuestionPrompt
	replies chan asktool.Reply
	opened  uint64
	refused map[<-chan Update]struct{}
	timer   *time.Timer
}

func (t *Thread) Ask(ctx context.Context, callID string, questions []asktool.Question) (asktool.Reply, error) {
	if raw, err := url.PathUnescape(callID); err == nil {
		callID = raw
	}
	select {
	case <-ctx.Done():
		return asktool.Reply{Outcome: asktool.OutcomeInterrupted}, nil
	default:
	}
	masked, err := t.host.maskQuestions(questions)
	if err != nil {
		return asktool.Reply{}, err
	}
	prompt := QuestionPrompt{RequestID: newID("req-"), CallID: callID, Questions: masked}
	replies := make(chan asktool.Reply, 1)
	offered, err := ask(ctx, t, func(a *actor) bool { return a.offerQuestion(prompt, replies) })
	if err != nil {
		if ctx.Err() != nil {
			return asktool.Reply{Outcome: asktool.OutcomeInterrupted}, nil
		}
		return asktool.Reply{}, err
	}
	if !offered {
		return asktool.Reply{Outcome: asktool.OutcomeNoClient}, nil
	}
	return t.awaitQuestion(ctx, prompt.RequestID, replies), nil
}

func (t *Thread) AnswerQuestion(requestID string, reply asktool.Reply) error {
	return askErr(context.Background(), t, func(a *actor) error {
		if !a.resolveQuestion(requestID, reply) {
			return fmt.Errorf("%w: %s", ErrUnknownRequest, requestID)
		}
		return nil
	})
}

func (t *Thread) CannotAnswerQuestion(requestID string, window <-chan Update) error {
	return askErr(context.Background(), t, func(a *actor) error {
		pending, ok := a.questions[requestID]
		if !ok {
			return fmt.Errorf("%w: %s", ErrUnknownRequest, requestID)
		}
		if !a.attached(window) {
			return nil
		}
		pending.refused[window] = struct{}{}
		a.settleQuestions()
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
		masked := asktool.Question{Header: mask(question.Header), Text: mask(question.Text), MultiSelect: question.MultiSelect, NoOther: question.NoOther}
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
	pending := &pendingQuestion{prompt: prompt, replies: replies, opened: a.questionSeq, refused: map[<-chan Update]struct{}{}}
	a.questions[prompt.RequestID] = pending
	a.publish(Update{Kind: UpdateQuestion, Thread: a.t.id, Question: prompt})
	pending.timer = time.AfterFunc(asktool.Timeout, func() { a.t.expireQuestion(prompt.RequestID) })
	return true
}

func (t *Thread) awaitQuestion(ctx context.Context, requestID string, replies chan asktool.Reply) asktool.Reply {
	select {
	case reply := <-replies:
		return reply
	case <-ctx.Done():
	}
	_, _ = ask(ctx, t, func(a *actor) bool {
		return a.resolveQuestion(requestID, asktool.Reply{Outcome: asktool.OutcomeInterrupted})
	})
	select {
	case reply := <-replies:
		return reply
	default:
		return asktool.Reply{Outcome: asktool.OutcomeInterrupted}
	}
}

func (t *Thread) expireQuestion(requestID string) {
	_ = t.do(context.Background(), func(a *actor) { a.resolveQuestion(requestID, asktool.Reply{Outcome: asktool.OutcomeTimedOut}) })
}

func (a *actor) resolveQuestion(requestID string, reply asktool.Reply) bool {
	pending, ok := a.questions[requestID]
	if !ok {
		return false
	}
	if pending.timer != nil {
		pending.timer.Stop()
	}
	delete(a.questions, requestID)
	pending.replies <- reply
	prompt := pending.prompt
	prompt.Resolved = true
	prompt.Resolution = reply.Outcome
	a.publish(Update{Kind: UpdateQuestion, Thread: a.t.id, Question: prompt})
	return true
}

func (a *actor) settleQuestions() {
	for requestID, pending := range a.questions {
		if a.allRefused(pending.refused) {
			a.resolveQuestion(requestID, asktool.Reply{Outcome: asktool.OutcomeNoClient})
		}
	}
}

func (a *actor) allRefused(refused map[<-chan Update]struct{}) bool {
	for ch := range a.subs {
		if _, ok := refused[ch]; !ok {
			return false
		}
	}
	return true
}

func (a *actor) attached(window <-chan Update) bool {
	for ch := range a.subs {
		if ch == window {
			return true
		}
	}
	return false
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
		a.drop(ch)
		a.t.host.log.Warn("agent: closing slow subscriber", "thread", a.t.id)
	}
}

func (a *actor) drop(ch chan Update) {
	close(ch)
	delete(a.subs, ch)
	for _, pending := range a.questions {
		delete(pending.refused, ch)
	}
	for _, pending := range a.approvals {
		delete(pending.refused, ch)
	}
}
