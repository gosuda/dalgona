package agent

import (
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
)

// EffectiveSandbox reports the confinement the host enforces.
// A configured workspace-write policy without a harness sandbox is off.
func (t *Thread) EffectiveSandbox() (config.SandboxMode, error) {
	settings, err := t.Settings()
	if err != nil {
		return "", err
	}
	if settings.Approval == config.ApprovalYolo {
		return config.SandboxOff, nil
	}
	if settings.Sandbox != config.SandboxOff && !runner.Available() {
		return config.SandboxOff, nil
	}
	return settings.Sandbox, nil
}

// SandboxAvailable reports whether the harness sandbox is active.
func (t *Thread) SandboxAvailable() bool {
	return runner.Available()
}
