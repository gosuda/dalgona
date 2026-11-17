package scripthost_test

import (
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
)

const (
	hangPlugin       = "def hang(ctx, event):\n    time.sleep(3600)\non('tool_call', hang)\n"
	laterDenyPlugin  = "def guard(ctx, event):\n    return {'decision': 'deny', 'reason': 'later hook denies'}\non('tool_call', guard)\n"
	hookLimit        = 500 * time.Millisecond
	hookTimeoutError = "plugin hang.star hook timed out after 500ms"
)

type lockedBuffer struct {
	mu  sync.Mutex
	buf strings.Builder
}

func (b *lockedBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *lockedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

func withHookPlugins(t *testing.T, files map[string]string, logs *lockedBuffer) func(*harness) {
	return func(h *harness) {
		h.scripts.Plugins = loadPlugins(t, files)
		h.scripts.HookTimeout = hookLimit
		h.scripts.Core.Mode = config.ToolModeNormal
		h.cfg.Config.Approval = config.ApprovalAsk
		h.cfg.Logger = slog.New(slog.NewTextHandler(logs, nil))
		if err := os.WriteFile(filepath.Join(h.cwd, "notes.txt"), []byte("hook deadline bytes"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
}

func TestHookDeadlineLetsTheCallProceedAfterATimeout(t *testing.T) {
	t.Parallel()
	logs := &lockedBuffer{}
	h := newHarness(t, withHookPlugins(t, map[string]string{"hang.star": hangPlugin}, logs))
	h.model.push(toolCall("x", "read", `{"path":"notes.txt"}`), text("done"))
	events := h.idle(h.run("read it"))
	if resolved := decisions(events)["x"]; resolved.Decision != "accept" || resolved.By != "mode:ask" {
		t.Fatalf("decision after the hook timeout = %+v, want the mechanical mode:ask accept", resolved)
	}
	if got := outputs(events)["x"].Output; !strings.Contains(got, "1:hook deadline bytes") {
		t.Fatalf("read output = %s", got)
	}
	if !strings.Contains(logs.String(), hookTimeoutError) {
		t.Fatalf("the gate did not log %q; log = %s", hookTimeoutError, logs.String())
	}
}

func TestHookDeadlineKeepsALaterDenial(t *testing.T) {
	t.Parallel()
	logs := &lockedBuffer{}
	h := newHarness(t, withHookPlugins(t, map[string]string{"aa-hang.star": hangPlugin, "bb-deny.star": laterDenyPlugin}, logs))
	h.model.push(toolCall("x", "read", `{"path":"notes.txt"}`), text("done"))
	events := h.idle(h.run("read it"))
	resolved := decisions(events)["x"]
	if resolved.Decision != "decline" || resolved.By != "hook:bb-deny.star:guard" || resolved.Reason != "later hook denies" || count(events, event.TypeToolStarted) != 0 {
		t.Fatalf("decision = %+v, tools started = %d", resolved, count(events, event.TypeToolStarted))
	}
	if !strings.Contains(logs.String(), strings.Replace(hookTimeoutError, "hang.star", "aa-hang.star", 1)) {
		t.Fatalf("the gate did not log the hook timeout; log = %s", logs.String())
	}
}
