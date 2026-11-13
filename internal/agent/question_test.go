package agent

import (
	"context"
	"errors"
	"testing"
	"testing/synctest"
	"time"

	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

type questionResult struct {
	reply asktool.Reply
	err   error
}

func testQuestions() []asktool.Question {
	return []asktool.Question{{
		Header: "Database",
		Text:   "Which database?",
		Options: []asktool.Option{
			{Label: "Postgres", Description: "A server."},
			{Label: "SQLite", Description: "One file."},
		},
	}}
}

func askAsync(ctx context.Context, thread *Thread, callID string, questions []asktool.Question) <-chan questionResult {
	out := make(chan questionResult, 1)
	go func() {
		reply, err := thread.Ask(ctx, callID, questions)
		out <- questionResult{reply: reply, err: err}
	}()
	return out
}

func nextQuestionUpdate(t *testing.T, updates <-chan Update, resolved bool) QuestionPrompt {
	t.Helper()
	deadline := time.After(10 * time.Second)
	for {
		select {
		case update, ok := <-updates:
			if !ok {
				t.Fatal("subscriber closed before the question update")
			}
			if update.Kind == UpdateQuestion && update.Question.Resolved == resolved {
				return update.Question
			}
		case <-deadline:
			t.Fatalf("no question update with resolved=%v", resolved)
		}
	}
}

func waitQuestionResult(t *testing.T, results <-chan questionResult) asktool.Reply {
	t.Helper()
	select {
	case result := <-results:
		if result.err != nil {
			t.Fatalf("Ask: %v", result.err)
		}
		return result.reply
	case <-time.After(10 * time.Second):
		t.Fatal("Ask did not return")
	}
	return asktool.Reply{}
}

func TestAskReachesSubscriberAndReturnsAnswer(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	updates := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	prompt := nextQuestionUpdate(t, updates, false)
	if prompt.CallID != "call-1" || len(prompt.Questions) != 1 {
		t.Fatalf("question = %+v, want call-1 with one question", prompt)
	}
	answer := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"Postgres"}}}
	if err := thread.AnswerQuestion(prompt.RequestID, answer); err != nil {
		t.Fatalf("AnswerQuestion: %v", err)
	}
	reply := waitQuestionResult(t, results)
	if reply.Outcome != asktool.OutcomeAnswered || len(reply.Answers) != 1 || reply.Answers[0][0] != "Postgres" {
		t.Fatalf("reply = %+v, want the answer", reply)
	}
	if resolved := nextQuestionUpdate(t, updates, true); resolved.Resolution != asktool.OutcomeAnswered {
		t.Fatalf("resolution = %v, want answered", resolved.Resolution)
	}
}

func TestAskWithoutSubscriberDeclines(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	reply, err := thread.Ask(t.Context(), "call-1", testQuestions())
	if err != nil {
		t.Fatalf("Ask: %v", err)
	}
	if reply.Outcome != asktool.OutcomeNoClient {
		t.Fatalf("outcome = %v, want no client", reply.Outcome)
	}
}

func TestAskDeclinesWhenLastSubscriberLeaves(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	updates := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	nextQuestionUpdate(t, updates, false)
	thread.Unsubscribe(updates)
	if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeNoClient {
		t.Fatalf("outcome = %v, want no client", reply.Outcome)
	}
}

func TestAskStaysOpenWhileAnotherSubscriberRemains(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	first := thread.Subscribe()
	second := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	prompt := nextQuestionUpdate(t, first, false)
	thread.Unsubscribe(first)
	if _, err := thread.Status(); err != nil {
		t.Fatalf("Status: %v", err)
	}
	select {
	case result := <-results:
		t.Fatalf("question closed while a subscriber remained: %+v", result.reply)
	default:
	}
	if err := thread.AnswerQuestion(prompt.RequestID, asktool.Reply{Outcome: asktool.OutcomeAnswered}); err != nil {
		t.Fatalf("AnswerQuestion: %v", err)
	}
	if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeAnswered {
		t.Fatalf("outcome = %v, want answered", reply.Outcome)
	}
	nextQuestionUpdate(t, second, false)
}

func TestAskReplaysOpenQuestionToNewSubscriber(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	first := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	prompt := nextQuestionUpdate(t, first, false)
	second := thread.Subscribe()
	replayed := nextQuestionUpdate(t, second, false)
	if replayed.RequestID != prompt.RequestID {
		t.Fatalf("replayed request %q, want %q", replayed.RequestID, prompt.RequestID)
	}
	if err := thread.AnswerQuestion(prompt.RequestID, asktool.Reply{Outcome: asktool.OutcomeAnswered}); err != nil {
		t.Fatalf("AnswerQuestion: %v", err)
	}
	waitQuestionResult(t, results)
}

func TestAskInterruptResolvesInterrupted(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	updates := thread.Subscribe()
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	results := askAsync(ctx, thread, "call-1", testQuestions())
	nextQuestionUpdate(t, updates, false)
	cancel()
	if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeInterrupted {
		t.Fatalf("outcome = %v, want interrupted", reply.Outcome)
	}
	if resolved := nextQuestionUpdate(t, updates, true); resolved.Resolution != asktool.OutcomeInterrupted {
		t.Fatalf("resolution = %v, want interrupted", resolved.Resolution)
	}
}

func TestAnswerQuestionFirstAnswerWins(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	updates := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	prompt := nextQuestionUpdate(t, updates, false)
	first := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"Postgres"}}}
	if err := thread.AnswerQuestion(prompt.RequestID, first); err != nil {
		t.Fatalf("first AnswerQuestion: %v", err)
	}
	second := thread.AnswerQuestion(prompt.RequestID, asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"SQLite"}}})
	if !errors.Is(second, ErrUnknownRequest) {
		t.Fatalf("second AnswerQuestion = %v, want unknown request", second)
	}
	if reply := waitQuestionResult(t, results); reply.Answers[0][0] != "Postgres" {
		t.Fatalf("answer = %v, want the first answer", reply.Answers)
	}
	if resolved := nextQuestionUpdate(t, updates, true); resolved.Resolution != asktool.OutcomeAnswered {
		t.Fatalf("resolution = %v, want answered", resolved.Resolution)
	}
}

func TestStatusCountsOpenQuestions(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	updates := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	prompt := nextQuestionUpdate(t, updates, false)
	status, err := thread.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if status.Questions != 1 {
		t.Fatalf("open questions = %d, want 1", status.Questions)
	}
	if err = thread.AnswerQuestion(prompt.RequestID, asktool.Reply{Outcome: asktool.OutcomeCancelled}); err != nil {
		t.Fatalf("AnswerQuestion: %v", err)
	}
	waitQuestionResult(t, results)
	status, err = thread.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if status.Questions != 0 {
		t.Fatalf("open questions = %d, want 0", status.Questions)
	}
}

func TestQuestionDeclinesWhenEveryWindowCannotAnswer(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	first := thread.Subscribe()
	second := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	prompt := nextQuestionUpdate(t, first, false)
	nextQuestionUpdate(t, second, false)
	if err := thread.CannotAnswerQuestion(prompt.RequestID, first); err != nil {
		t.Fatalf("first CannotAnswerQuestion: %v", err)
	}
	if err := thread.CannotAnswerQuestion(prompt.RequestID, second); err != nil {
		t.Fatalf("second CannotAnswerQuestion: %v", err)
	}
	if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeNoClient {
		t.Fatalf("outcome = %v, want no client", reply.Outcome)
	}
}

func TestRefusalFromOneWindowCountsOnce(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	first := thread.Subscribe()
	second := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	prompt := nextQuestionUpdate(t, first, false)
	nextQuestionUpdate(t, second, false)
	for range 2 {
		if err := thread.CannotAnswerQuestion(prompt.RequestID, first); err != nil {
			t.Fatalf("CannotAnswerQuestion: %v", err)
		}
	}
	status, err := thread.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	if status.Questions != 1 {
		t.Fatalf("open questions = %d, want 1 while the second window can answer", status.Questions)
	}
	if err := thread.CannotAnswerQuestion(prompt.RequestID, second); err != nil {
		t.Fatalf("second CannotAnswerQuestion: %v", err)
	}
	if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeNoClient {
		t.Fatalf("outcome = %v, want no client", reply.Outcome)
	}
}

func TestRefusingWindowLeavingDoesNotDecline(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	first := thread.Subscribe()
	second := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	prompt := nextQuestionUpdate(t, first, false)
	nextQuestionUpdate(t, second, false)
	if err := thread.CannotAnswerQuestion(prompt.RequestID, first); err != nil {
		t.Fatalf("CannotAnswerQuestion: %v", err)
	}
	thread.Unsubscribe(first)
	answer := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"SQLite"}}}
	if err := thread.AnswerQuestion(prompt.RequestID, answer); err != nil {
		t.Fatalf("AnswerQuestion after the refusing window left: %v", err)
	}
	if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeAnswered {
		t.Fatalf("outcome = %v, want answered", reply.Outcome)
	}
}

func TestRefusalDoesNotDoubleCountAfterSlowClose(t *testing.T) {
	t.Parallel()
	thread := newHarness(t).start()
	first := thread.Subscribe()
	second := thread.Subscribe()
	results := askAsync(t.Context(), thread, "call-1", testQuestions())
	prompt := nextQuestionUpdate(t, first, false)
	nextQuestionUpdate(t, second, false)
	if err := thread.CannotAnswerQuestion(prompt.RequestID, first); err != nil {
		t.Fatalf("CannotAnswerQuestion: %v", err)
	}
	flood(thread, second)
	requireClosed(t, first)
	resubscribed := thread.Subscribe()
	if replayed := nextQuestionUpdate(t, resubscribed, false); replayed.RequestID != prompt.RequestID {
		t.Fatalf("replayed request %q, want %q", replayed.RequestID, prompt.RequestID)
	}
	if err := thread.CannotAnswerQuestion(prompt.RequestID, resubscribed); err != nil {
		t.Fatalf("re-asked CannotAnswerQuestion: %v", err)
	}
	answer := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"Postgres"}}}
	if err := thread.AnswerQuestion(prompt.RequestID, answer); err != nil {
		t.Fatalf("question closed after the re-asked window refused: %v", err)
	}
	if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeAnswered {
		t.Fatalf("outcome = %v, want answered", reply.Outcome)
	}
}

func TestOfferKeepsQuestionWhenItClosesSoleWindowAsSlow(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		thread := newHarness(t).start()
		thread.Subscribe()
		for range subscriberBuffer {
			thread.publish(Update{Kind: UpdateExecOutput, CallID: "flood"})
		}
		results := askAsync(t.Context(), thread, "call-1", testQuestions())
		synctest.Wait()
		resubscribed := thread.Subscribe()
		prompt := nextQuestionUpdate(t, resubscribed, false)
		answer := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"SQLite"}}}
		if err := thread.AnswerQuestion(prompt.RequestID, answer); err != nil {
			t.Fatalf("question closed when its offer closed the sole window as slow: %v", err)
		}
		if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeAnswered {
			t.Fatalf("outcome = %v, want answered", reply.Outcome)
		}
	})
}

func flood(thread *Thread, keep <-chan Update) {
	for range subscriberBuffer + 1 {
		thread.publish(Update{Kind: UpdateExecOutput, CallID: "flood"})
		_, _ = thread.Status()
		for drained := false; !drained; {
			select {
			case <-keep:
			default:
				drained = true
			}
		}
	}
}

func requireClosed(t *testing.T, updates <-chan Update) {
	t.Helper()
	deadline := time.After(10 * time.Second)
	for {
		select {
		case _, ok := <-updates:
			if !ok {
				return
			}
		case <-deadline:
			t.Fatal("subscriber was not closed as slow")
		}
	}
}
