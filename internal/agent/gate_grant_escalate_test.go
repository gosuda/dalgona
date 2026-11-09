package agent

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestAskSessionGrantBindsEscalation(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalAsk))
	h.model.push(
		calls(call{id: "a", name: "exec", args: `{"command":"go test ./..."}`}),
		calls(call{id: "b", name: "exec", args: `{"command":"go test -run X","escalate":true}`}),
		text("done"),
	)
	thread := h.start()
	client := answerApprovals(t, thread, func(prompt ApprovalPrompt) Reply {
		return ReplyAcceptSession
	})
	h.submit(thread, "test then escalated test")
	h.waitIdle(thread)
	prompts := client.stop()
	got := resolutions(h.events(thread.ID()))
	if len(prompts) != 2 {
		t.Fatalf("prompts = %+v, want one prompt per call", prompts)
	}
	if got["b"].By != "user" {
		t.Fatalf("resolutions = %+v, want the escalated call resolved by user", got)
	}
	var escalated *ApprovalPrompt
	for i := range prompts {
		if prompts[i].CallID == "b" {
			escalated = &prompts[i]
		}
	}
	if escalated == nil {
		t.Fatal("no prompt recorded for the escalated call")
	}
	if !strings.Contains(escalated.Summary, "unsandboxed") {
		t.Fatalf("prompt summary = %q, want the unsandboxed marker", escalated.Summary)
	}
	if !strings.Contains(escalated.Summary, h.cwd) {
		t.Fatalf("prompt summary = %q, want the thread cwd", escalated.Summary)
	}
}
