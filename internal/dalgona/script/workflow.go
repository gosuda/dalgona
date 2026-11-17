package script

import (
	"context"
	"encoding/json/jsontext"
	"fmt"

	corescript "github.com/gosuda/dalgona/internal/dal/script"
)

type Workflow struct {
	ID     string
	Entry  string
	Source []byte
	Args   jsontext.Value
}

func RunWorkflow(ctx context.Context, fx corescript.Effects, w Workflow) (corescript.Outcome, error) {
	start, done, err := fx.Started(corescript.RunStart{ID: w.ID, Entry: w.Entry, Source: w.Source, Args: w.Args, Seed: corescript.NewSeed()})
	if err != nil {
		return corescript.Outcome{}, fmt.Errorf("workflow %s: start run: %w", w.ID, err)
	}
	if done != nil {
		return *done, nil
	}
	return corescript.Complete(context.WithoutCancel(ctx), fx, start.ID, runWorkflow(ctx, fx, start))
}

func runWorkflow(ctx context.Context, fx corescript.Effects, start corescript.RunStart) corescript.Outcome {
	r, env, end := corescript.NewRuntime(ctx, fx, start.ID, start.Seed)
	defer end()
	args, err := corescript.FromJSON(start.Args)
	if err != nil {
		return corescript.Outcome{Status: corescript.StatusFailed, Error: corescript.Failure{Code: "invalid_args", Message: "workflow args: " + err.Error(), Stage: "args"}}
	}
	env = r.WithContext(env)
	env["args"] = args
	value, err := r.ExecCell(ctx, start.Entry, start.Source, env)
	return r.Finish(ctx, value, err)
}
