package agent

import (
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
)

func nextApprovalPrompt(t *testing.T, updates <-chan Update) ApprovalPrompt {
	t.Helper()
	deadline := time.After(10 * time.Second)
	for {
		select {
		case update, ok := <-updates:
			if !ok {
				t.Fatal("subscriber closed before the approval update")
			}
			if update.Kind == UpdateApproval && !update.Approval.Resolved {
				return update.Approval
			}
		case <-deadline:
			t.Fatal("no approval update")
		}
	}
}

func TestApprovalDeclinesWhenEveryAttachedWindowRefuses(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalAsk))
	h.model.push(calls(call{id: "x", name: "exec", args: `{"command":"go version"}`}), text("done"))
	thread := h.start()
	first := thread.Subscribe()
	second := thread.Subscribe()
	h.submit(thread, "run it")
	prompt := nextApprovalPrompt(t, first)
	nextApprovalPrompt(t, second)
	if err := thread.CannotAnswerApproval(prompt.RequestID, first); err != nil {
		t.Fatalf("first CannotAnswerApproval: %v", err)
	}
	if err := thread.CannotAnswerApproval(prompt.RequestID, second); err != nil {
		t.Fatalf("second CannotAnswerApproval: %v", err)
	}
	h.waitIdle(thread)
	events := h.events(thread.ID())
	if resolved := resolutions(events)["x"]; resolved.Decision != "decline" || started(events, "x") {
		t.Fatalf("resolved = %+v, want the call declined", resolved)
	}
}

func TestApprovalStaysOpenWhileAnotherWindowCanAnswer(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalAsk))
	h.model.push(calls(call{id: "x", name: "exec", args: `{"command":"go version"}`}), text("done"))
	thread := h.start()
	first := thread.Subscribe()
	second := thread.Subscribe()
	h.submit(thread, "run it")
	prompt := nextApprovalPrompt(t, first)
	nextApprovalPrompt(t, second)
	if err := thread.CannotAnswerApproval(prompt.RequestID, first); err != nil {
		t.Fatalf("CannotAnswerApproval: %v", err)
	}
	if err := thread.Answer(prompt.RequestID, ReplyAccept); err != nil {
		t.Fatalf("Answer after one refusal: %v", err)
	}
	h.waitIdle(thread)
	events := h.events(thread.ID())
	if resolved := resolutions(events)["x"]; resolved.Decision != "accept" || !started(events, "x") {
		t.Fatalf("resolved = %+v, want the call accepted by the second window", resolved)
	}
}

func TestApprovalDeclinesAfterRefusingWindowAndLastWindowLeave(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalAsk))
	h.model.push(calls(call{id: "x", name: "exec", args: `{"command":"go version"}`}), text("done"))
	thread := h.start()
	first := thread.Subscribe()
	second := thread.Subscribe()
	h.submit(thread, "run it")
	prompt := nextApprovalPrompt(t, first)
	nextApprovalPrompt(t, second)
	if err := thread.CannotAnswerApproval(prompt.RequestID, first); err != nil {
		t.Fatalf("CannotAnswerApproval: %v", err)
	}
	thread.Unsubscribe(second)
	thread.Unsubscribe(first)
	h.waitIdle(thread)
	events := h.events(thread.ID())
	if resolved := resolutions(events)["x"]; resolved.Decision != "decline" || started(events, "x") {
		t.Fatalf("resolved = %+v, want the call declined when the windows left", resolved)
	}
}
