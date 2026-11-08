package remote

import "github.com/gosuda/dalgona/internal/agent"

var (
	_ Recorder            = (*agent.Host)(nil)
	_ agent.RunnerFactory = (*Binder)(nil).Runner
)
