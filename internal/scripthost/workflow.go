package scripthost

import (
	"cmp"
	"context"
	"log/slog"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/script"
)

func RunWorkflow(ctx context.Context, t *agent.Thread, cfg Config, w script.Workflow) (script.Outcome, error) {
	fx := effects{t: t, judge: cfg.Judge, log: cmp.Or(cfg.Logger, slog.Default()), idx: newEffectCache()}
	var outcome script.Outcome
	err := t.RunScript(ctx, w.Entry, func(ctx context.Context) error {
		var runErr error
		outcome, runErr = script.RunWorkflow(ctx, fx, w)
		return runErr
	})
	return outcome, err
}

func (e extension) ResumeScript(ctx context.Context, runID string) error {
	_, err := script.RunWorkflow(ctx, e.fx, script.Workflow{ID: runID})
	return err
}
