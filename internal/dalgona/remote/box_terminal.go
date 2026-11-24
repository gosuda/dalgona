package remote

import (
	"context"

	"github.com/gosuda/dalgona/internal/dal/runner"
)

var _ runner.TerminalOpener = (*boxRunner)(nil)

func (r *boxRunner) OpenTerminal(ctx context.Context, spec runner.TerminalSpec) (runner.Terminal, error) {
	var opened runner.Terminal
	err := r.do(ctx, func(l *link) error {
		boxed := spec
		boxed.Dir = l.boxPath(spec.Dir)
		term, err := l.remote.OpenTerminal(ctx, boxed)
		opened = term
		return err
	})
	return opened, err
}
