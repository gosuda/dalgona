package agent

import (
	"context"
	"errors"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

func openQuestion(t *testing.T, thread *Thread, updates <-chan Update) QuestionPrompt {
	t.Helper()
	return nextQuestionUpdate(t, updates, false)
}

func drainOneUpdate(updates <-chan Update) (Update, bool) {
	select {
	case update, ok := <-updates:
		return update, ok
	default:
		return Update{}, false
	}
}

func assertNoFurtherUpdate(t *testing.T, updates <-chan Update) {
	t.Helper()
	if update, ok := drainOneUpdate(updates); ok {
		t.Fatalf("extra update after resolution: %+v", update)
	}
}

func questionCount(t *testing.T, thread *Thread) int {
	t.Helper()
	status, err := thread.Status()
	if err != nil {
		t.Fatalf("Status: %v", err)
	}
	return status.Questions
}

func readyQuestionResult(t *testing.T, results <-chan questionResult) asktool.Reply {
	t.Helper()
	select {
	case result := <-results:
		if result.err != nil {
			t.Fatalf("Ask: %v", result.err)
		}
		return result.reply
	default:
		t.Fatal("Ask had not returned when the timeout elapsed")
	}
	return asktool.Reply{}
}

func TestQuestionTimesOutExactlyAtTimeout(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		thread := newHarness(t).start()
		updates := thread.Subscribe()
		results := askAsync(t.Context(), thread, testQuestions())
		prompt := openQuestion(t, thread, updates)
		time.Sleep(5*time.Minute - time.Nanosecond)
		synctest.Wait()
		if count := questionCount(t, thread); count != 1 {
			t.Fatalf("open questions before the timeout = %d, want 1", count)
		}
		assertNoFurtherUpdate(t, updates)
		time.Sleep(time.Nanosecond)
		synctest.Wait()
		reply := readyQuestionResult(t, results)
		if reply.Outcome != asktool.OutcomeTimedOut {
			t.Fatalf("outcome = %v, want timed out", reply.Outcome)
		}
		resolved, ok := drainOneUpdate(updates)
		if !ok || resolved.Kind != UpdateQuestion || !resolved.Question.Resolved {
			t.Fatalf("no resolved question update at the timeout, got %+v ok=%v", resolved, ok)
		}
		if resolved.Question.RequestID != prompt.RequestID || resolved.Question.Resolution != asktool.OutcomeTimedOut {
			t.Fatalf("resolved update = %+v, want %s timed out", resolved.Question, prompt.RequestID)
		}
		if count := questionCount(t, thread); count != 0 {
			t.Fatalf("open questions after the timeout = %d, want 0", count)
		}
		assertNoFurtherUpdate(t, updates)
	})
}

func TestAnswerOneTickBeforeTimeoutWins(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		thread := newHarness(t).start()
		updates := thread.Subscribe()
		results := askAsync(t.Context(), thread, testQuestions())
		prompt := openQuestion(t, thread, updates)
		time.Sleep(5*time.Minute - time.Nanosecond)
		answer := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"Postgres"}}}
		if err := thread.AnswerQuestion(prompt.RequestID, answer); err != nil {
			t.Fatalf("AnswerQuestion: %v", err)
		}
		if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeAnswered || reply.Answers[0][0] != "Postgres" {
			t.Fatalf("reply = %+v, want the answer", reply)
		}
		nextQuestionUpdate(t, updates, true)
		time.Sleep(10 * time.Minute)
		synctest.Wait()
		if count := questionCount(t, thread); count != 0 {
			t.Fatalf("open questions after a late fire = %d, want 0", count)
		}
		assertNoFurtherUpdate(t, updates)
	})
}

func TestTimeoutStopsOnCancelInterruptAndLastWindowLeaving(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		want asktool.Outcome
		end  func(in questionEndInput)
	}{
		{
			name: "cancel reply",
			want: asktool.OutcomeCancelled,
			end: func(in questionEndInput) {
				if err := in.thread.AnswerQuestion(in.prompt.RequestID, asktool.Reply{Outcome: asktool.OutcomeCancelled}); err != nil {
					in.t.Fatalf("AnswerQuestion: %v", err)
				}
			},
		},
		{
			name: "turn context cancel",
			want: asktool.OutcomeInterrupted,
			end:  func(in questionEndInput) { in.cancel() },
		},
		{
			name: "last window leaves",
			want: asktool.OutcomeNoClient,
			end:  func(in questionEndInput) { in.thread.Unsubscribe(in.updates) },
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			requireQuestionEndsWithoutLateFire(t, tc.end, tc.want)
		})
	}
}

type questionEndInput struct {
	t       *testing.T
	thread  *Thread
	updates <-chan Update
	prompt  QuestionPrompt
	cancel  context.CancelFunc
}

func requireQuestionEndsWithoutLateFire(t *testing.T, end func(in questionEndInput), want asktool.Outcome) {
	t.Helper()
	synctest.Test(t, func(t *testing.T) {
		thread := newHarness(t).start()
		ctx, cancel := context.WithCancel(t.Context())
		defer cancel()
		updates := thread.Subscribe()
		results := askAsync(ctx, thread, testQuestions())
		prompt := openQuestion(t, thread, updates)
		end(questionEndInput{t: t, thread: thread, updates: updates, prompt: prompt, cancel: cancel})
		if reply := waitQuestionResult(t, results); reply.Outcome != want {
			t.Fatalf("outcome = %v, want %v", reply.Outcome, want)
		}
		time.Sleep(6 * time.Minute)
		synctest.Wait()
		if count := questionCount(t, thread); count != 0 {
			t.Fatalf("open questions after a late fire = %d, want 0", count)
		}
	})
}

func TestLateSubscriberDoesNotRestartTimeout(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		thread := newHarness(t).start()
		first := thread.Subscribe()
		results := askAsync(t.Context(), thread, testQuestions())
		prompt := openQuestion(t, thread, first)
		time.Sleep(2 * time.Minute)
		second := thread.Subscribe()
		replayed := nextQuestionUpdate(t, second, false)
		if replayed.RequestID != prompt.RequestID {
			t.Fatalf("replayed request = %s, want %s", replayed.RequestID, prompt.RequestID)
		}
		time.Sleep(3 * time.Minute)
		synctest.Wait()
		reply := waitQuestionResult(t, results)
		if reply.Outcome != asktool.OutcomeTimedOut {
			t.Fatalf("outcome = %v, want timed out at 5 minutes from the offer", reply.Outcome)
		}
		if resolved := nextQuestionUpdate(t, first, true); resolved.Resolution != asktool.OutcomeTimedOut {
			t.Fatalf("first window resolution = %v, want timed out", resolved.Resolution)
		}
		if resolved := nextQuestionUpdate(t, second, true); resolved.Resolution != asktool.OutcomeTimedOut {
			t.Fatalf("late window resolution = %v, want timed out", resolved.Resolution)
		}
	})
}

func TestAnswerAfterTimeoutIsUnknownRequest(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		thread := newHarness(t).start()
		updates := thread.Subscribe()
		results := askAsync(t.Context(), thread, testQuestions())
		prompt := openQuestion(t, thread, updates)
		time.Sleep(5 * time.Minute)
		synctest.Wait()
		if reply := waitQuestionResult(t, results); reply.Outcome != asktool.OutcomeTimedOut {
			t.Fatalf("outcome = %v, want timed out", reply.Outcome)
		}
		answer := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"Postgres"}}}
		err := thread.AnswerQuestion(prompt.RequestID, answer)
		if !errors.Is(err, ErrUnknownRequest) {
			t.Fatalf("AnswerQuestion after expiry = %v, want %v", err, ErrUnknownRequest)
		}
		select {
		case result := <-results:
			t.Fatalf("extra Ask result after the late answer = %+v", result)
		default:
		}
	})
}

func TestSandboxChoiceTimeoutMakesNoChoice(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
		h.model.push(calls(call{id: "c1", name: "exec", args: `{"command":"echo one"}`}), calls(call{id: "c2", name: "exec", args: `{"command":"echo two"}`}), text("done"))
		thread := h.start()
		updates := thread.Subscribe()
		h.submit(thread, "run two commands")
		first := nextQuestionUpdate(t, updates, false)
		time.Sleep(5 * time.Minute)
		synctest.Wait()
		second := nextQuestionUpdate(t, updates, false)
		if second.RequestID == first.RequestID {
			t.Fatalf("timed-out sandbox question asked again under the same request id %s", first.RequestID)
		}
		if got := callOutputs(h.model.request(1))["c1"]; got != "declined: no sandbox choice was made" {
			t.Fatalf("timed-out command output = %q, want the no-choice decline", got)
		}
		if chosen := sandboxChoices(h.events(thread.ID())); len(chosen) != 0 {
			t.Fatalf("sandbox choices after a timeout = %+v, want none", chosen)
		}
		answerSandbox(t, thread, second)
		approval := nextApprovalPrompt(t, updates)
		if err := thread.Answer(approval.RequestID, ReplyDecline); err != nil {
			t.Fatalf("Answer: %v", err)
		}
		h.waitIdle(thread)
		chosen := sandboxChoices(h.events(thread.ID()))
		if len(chosen) != 1 || chosen[0].Choice != event.SandboxChoiceAccept {
			t.Fatalf("sandbox choices = %+v, want exactly the accept from the second question", chosen)
		}
	})
}
