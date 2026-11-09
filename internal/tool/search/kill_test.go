package search

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type failReadRunner struct {
	runner.Runner
	started bool
	signals []runner.Signal
}

func (r *failReadRunner) Start(ctx context.Context, spec runner.ProcessSpec) error {
	r.started = true
	return r.Runner.Start(ctx, spec)
}

func (r *failReadRunner) Read(ctx context.Context, id string, offset int64) (runner.Output, error) {
	if offset > 0 {
		return runner.Output{}, errors.New("injected read failure")
	}
	return r.Runner.Read(ctx, id, offset)
}

func (r *failReadRunner) Signal(ctx context.Context, id string, signal runner.Signal) error {
	r.signals = append(r.signals, signal)
	return r.Runner.Signal(ctx, id, signal)
}

func TestRunKillsSubprocessWhenCollectFails(t *testing.T) {
	env, _ := searchTestEnv(t)
	wrapped := &failReadRunner{Runner: env.Runner}
	env.Runner = wrapped
	_, err := Tool{}.Run(t.Context(), tool.Call{ID: "search-kill", Args: jsontext.Value(`{"mode":"grep","pattern":"x"}`)}, env)
	if err == nil {
		t.Fatal("collect failure returned no error")
	}
	if !wrapped.started {
		t.Fatal("subprocess was not started")
	}
	if !slices.Contains(wrapped.signals, runner.SignalKill) {
		t.Fatalf("kill signal missing, got %v", wrapped.signals)
	}
}

func searchTestEnv(t *testing.T) (tool.Env, string) {
	t.Helper()
	cwd := t.TempDir()
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	return tool.Env{Runner: local, Cwd: cwd, Home: t.TempDir(), Snapshots: snapshot.New()}, cwd
}
