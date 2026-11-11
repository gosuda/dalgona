package agent

import (
	"context"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
)

func approvingTools(calls *atomic.Int32) func(context.Context, string, string) ([]tool.Tool, error) {
	return func(ctx context.Context, _, cwd string) ([]tool.Tool, error) {
		calls.Add(1)
		thread, ok := ThreadFromContext(ctx)
		if !ok {
			return []tool.Tool{}, nil
		}
		reply, err := thread.RequestWorkspaceApproval(ctx, WorkspaceApprovalRequest{
			Cwd:     cwd,
			Servers: []WorkspaceServer{{Name: "wsfix", Command: "/tmp/srv"}},
		})
		if err != nil {
			return nil, err
		}
		if reply != ReplyAccept {
			return []tool.Tool{}, nil
		}
		return []tool.Tool{}, nil
	}
}

func openPendingTrustThread(t *testing.T, h *harness) *Thread {
	t.Helper()
	type openResult struct {
		thread *Thread
		err    error
	}
	opened := make(chan openResult, 1)
	go func() {
		thread, err := h.host.Start(t.Context(), ThreadSpec{})
		opened <- openResult{thread: thread, err: err}
	}()
	select {
	case out := <-opened:
		if out.err != nil {
			t.Fatalf("open: %v", out.err)
		}
		return out.thread
	case <-time.After(10 * time.Second):
		t.Fatal("open blocked on workspace trust; trust must wait for the first turn")
		return nil
	}
}

func approveFirstTurnTools(t *testing.T, thread *Thread, model string) *toolset {
	t.Helper()
	updates := thread.Subscribe()
	defer thread.Unsubscribe(updates)
	type toolsResult struct {
		set *toolset
		err error
	}
	discovered := make(chan toolsResult, 1)
	go func() {
		set, err := thread.toolsFor(t.Context(), model)
		discovered <- toolsResult{set: set, err: err}
	}()
	prompt := awaitApprovalPrompt(t, updates)
	checkPromptContains(t, prompt, []string{"/tmp/srv"})
	if err := thread.Answer(prompt.RequestID, ReplyAccept); err != nil {
		t.Fatal(err)
	}
	select {
	case out := <-discovered:
		if out.err != nil {
			t.Fatalf("first-turn tools: %v", out.err)
		}
		return out.set
	case <-time.After(10 * time.Second):
		t.Fatal("first-turn discovery did not finish after approval")
		return nil
	}
}

func TestOpenDefersWorkspaceTrustToFirstTurn(t *testing.T) {
	t.Parallel()
	var calls atomic.Int32
	h := newHarness(t, func(h *harness) {
		h.cfg.Tools = approvingTools(&calls)
	})
	thread := openPendingTrustThread(t, h)
	if got := calls.Load(); got != 0 {
		t.Fatalf("tools built at open = %d, want none before the first turn", got)
	}
	if set := approveFirstTurnTools(t, thread, h.cfg.Config.Model); set == nil {
		t.Fatal("first-turn tools = nil, want a toolset")
	}
	if got := calls.Load(); got != 1 {
		t.Fatalf("tools builds = %d, want exactly one at the first turn", got)
	}
}

func firstPromptNotices(t *testing.T, thread *Thread) []event.Notice {
	t.Helper()
	v, err := thread.view()
	if err != nil {
		t.Fatal(err)
	}
	for _, item := range v.records {
		if msg, ok := item.Event.(event.UserMessage); ok && len(msg.Notices) > 0 {
			return msg.Notices
		}
	}
	return nil
}

func TestFirstPromptKeepsToolKeywordNotice(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) {
		h.cfg.Config.Keywords = map[string]config.KeywordSpec{
			"deepread": {Text: "Read the file before answering.", Requires: []string{"read"}, Enabled: true},
		}
	})
	thread := h.start()
	h.submit(thread, "please deepread the plan")
	for _, notice := range firstPromptNotices(t, thread) {
		if notice.Word == "deepread" {
			return
		}
	}
	t.Fatal("first prompt lost the deepread notice")
}
