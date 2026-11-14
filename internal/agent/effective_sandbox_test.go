package agent

import (
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func TestEffectiveSandboxReportsNoHarness(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withUnavailableSandbox(config.ApprovalAsk))
	thread := h.start()
	mode, err := thread.EffectiveSandbox()
	if err != nil {
		t.Fatal(err)
	}
	if mode != config.SandboxOff {
		t.Fatalf("effective sandbox = %q, want %q", mode, config.SandboxOff)
	}
}

func TestEffectiveSandboxReportsWorkspaceWrite(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) {
		h.cfg.Config.Sandbox = config.SandboxWorkspaceWrite
		h.cfg.SandboxCause = func() string { return "" }
	})
	thread := h.start()
	mode, err := thread.EffectiveSandbox()
	if err != nil {
		t.Fatal(err)
	}
	if mode != config.SandboxWorkspaceWrite {
		t.Fatalf("effective sandbox = %q, want %q", mode, config.SandboxWorkspaceWrite)
	}
}

func TestEffectiveSandboxIsOffUnderYolo(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Config.Sandbox = config.SandboxWorkspaceWrite
	})
	thread := h.start()
	mode, err := thread.EffectiveSandbox()
	if err != nil {
		t.Fatal(err)
	}
	if mode != config.SandboxOff {
		t.Fatalf("effective sandbox under yolo = %q, want %q", mode, config.SandboxOff)
	}
}
