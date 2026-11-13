package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
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
