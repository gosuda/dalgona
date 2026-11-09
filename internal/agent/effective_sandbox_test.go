package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestEffectiveSandboxReportsNoHarness(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) {
		h.cfg.Config.Sandbox = config.SandboxWorkspaceWrite
	})
	thread := h.start()
	mode, err := thread.EffectiveSandbox()
	if err != nil {
		t.Fatal(err)
	}
	if mode != config.SandboxWorkspaceWrite && mode != config.SandboxOff {
		t.Fatalf("effective sandbox = %q, want workspace-write or off", mode)
	}
}
