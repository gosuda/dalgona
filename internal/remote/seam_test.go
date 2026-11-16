package remote

import "github.com/gosuda/dalgona/internal/dal/agent"

var (
	_ Recorder            = (*agent.Host)(nil)
	_ agent.RunnerFactory = (*Binder)(nil).Runner
)
