package remote

import (
	"context"

	"github.com/gosuda/dalgona/internal/dal/runner"
)

var _ runner.Workspace = (*boxRunner)(nil)

func (r *boxRunner) Workspace(ctx context.Context, req runner.WorkspaceRequest) (runner.WorkspaceResult, error) {
	var result runner.WorkspaceResult
	call := func(l *link) error {
		boxed := req
		boxed.Root = l.boxPath(req.Root)
		var err error
		result, err = l.remote.Workspace(ctx, boxed)
		return err
	}
	if req.Op.Reads() {
		return result, r.do(ctx, call)
	}
	l, err := r.ensure(ctx)
	if err != nil {
		return result, err
	}
	return result, call(l)
}
