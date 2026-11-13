package agent

import "github.com/gosuda/dalgona/internal/config"

func (t *Thread) EffectiveSandbox() (config.SandboxMode, error) {
	settings, err := t.Settings()
	if err != nil {
		return "", err
	}
	if settings.Approval == config.ApprovalYolo {
		return config.SandboxOff, nil
	}
	if settings.Sandbox != config.SandboxOff && t.host.cfg.SandboxCause() != "" {
		return config.SandboxOff, nil
	}
	return settings.Sandbox, nil
}
