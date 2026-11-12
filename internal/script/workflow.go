package script

import (
	"context"
	"encoding/json/jsontext"
	"fmt"
)

type Workflow struct {
	ID     string
	Entry  string
	Source []byte
	Args   jsontext.Value
}

func RunWorkflow(ctx context.Context, fx Effects, w Workflow) (Outcome, error) {
	start, done, err := fx.Started(RunStart{ID: w.ID, Entry: w.Entry, Source: w.Source, Args: w.Args, Seed: newSeed()})
	if err != nil {
		return Outcome{}, fmt.Errorf("workflow %s: start run: %w", w.ID, err)
	}
	if done != nil {
		return *done, nil
	}
	return complete(fx, start.ID, runWorkflow(ctx, fx, start))
}

func runWorkflow(ctx context.Context, fx Effects, start RunStart) Outcome {
	r := newRun(fx, start.ID, runConfig{})
	env, end := r.begin(ctx, start.Seed)
	defer end()
	args, err := fromJSON(start.Args)
	if err != nil {
		return stopped(StatusFailed, codeInvalid, "workflow args: "+err.Error(), stageArgs)
	}
	env = r.withContext(env)
	env["args"] = args
	value, err := r.execCell(start.Entry, start.Source, env)
	return r.finish(ctx, value, err)
}
