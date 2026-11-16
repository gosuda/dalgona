package agent

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/tool"
)

func TestHumanDeclinesWhenApprovalMaskingFails(t *testing.T) {
	authHome := t.TempDir()
	store, err := auth.NewWithPatterns(authHome, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) {
		h.cfg.Auth = store
	})
	thread := h.start()
	updates := thread.Subscribe()
	defer thread.Unsubscribe(updates)
	breakAuthHome(t, authHome)
	w := &worker{t: thread}
	gc := gateCall{
		call:  event.ToolCall{ID: "call-edit", Name: "patch"},
		class: tool.ClassEdit,
		files: []tool.FileChange{{Path: "a.txt", PreSHA: "a", PostSHA: "b", Pre: []byte("token-abcdefgh1234\n"), Post: []byte("two\n")}},
	}
	v := w.human(t.Context(), gc, "req-edit")
	if v.decision != "decline" || v.reason != "declined: secret masking failed" {
		t.Fatalf("verdict = %+v, want decline with secret masking failed", v)
	}
	for drained := false; !drained; {
		select {
		case update := <-updates:
			if update.Kind == UpdateApproval {
				t.Fatalf("approval prompt offered after masking failed: %+v", update.Approval)
			}
		default:
			drained = true
		}
	}
}

func TestDenialAndSkipOutputsAreMasked(t *testing.T) {
	store, err := auth.NewWithPatterns(t.TempDir(), []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) {
		h.cfg.Auth = store
	})
	thread := h.start()
	if err := thread.Append(t.Context(), event.TurnStarted{TurnID: "t"}, startedCall("call-skip", "read", "read", `{}`), startedCall("call-deny", "exec", "exec", `{}`)); err != nil {
		t.Fatal(err)
	}
	w := &worker{t: thread}
	reason := "not allowed: token-abcdefgh1234"
	if err := w.skip(t.Context(), []event.ToolCall{{ID: "call-skip", Name: "read"}}, reason); err != nil {
		t.Fatal(err)
	}
	if err := w.declineCall(t.Context(), event.ToolCall{ID: "call-deny", Name: "exec"}, "", verdict{decision: "decline", by: "rule", reason: reason}); err != nil {
		t.Fatal(err)
	}
	for _, id := range []string{"call-skip", "call-deny"} {
		output := completedOutputs(h.events(thread.ID()))[id].Output
		if strings.Contains(output, "token-abcdefgh1234") || !strings.Contains(output, "{{secret:") {
			t.Fatalf("%s output not masked: %q", id, output)
		}
	}
}

func TestDenialReasonIsMasked(t *testing.T) {
	store, err := auth.NewWithPatterns(t.TempDir(), []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) {
		h.cfg.Auth = store
	})
	thread := h.start()
	if err := thread.Append(t.Context(), event.TurnStarted{TurnID: "t"}, startedCall("call-deny", "exec", "exec", `{}`)); err != nil {
		t.Fatal(err)
	}
	w := &worker{t: thread}
	reason := "not allowed: token-abcdefgh1234"
	if err := w.declineCall(t.Context(), event.ToolCall{ID: "call-deny", Name: "exec"}, "", verdict{decision: "decline", by: "rule", reason: reason}); err != nil {
		t.Fatal(err)
	}
	for _, item := range h.events(thread.ID()) {
		if resolved, ok := item.Event.(event.ApprovalResolved); ok && resolved.Reason != "" {
			if strings.Contains(resolved.Reason, "token-abcdefgh1234") || !strings.Contains(resolved.Reason, "{{secret:") {
				t.Fatalf("resolved reason not masked: %q", resolved.Reason)
			}
		}
	}
}
