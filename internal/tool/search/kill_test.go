package search

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"runtime"
	"slices"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type failReadRunner struct {
	runner.Runner
	started bool
	signals []runner.Signal
	reads   int
}

func (r *failReadRunner) Start(ctx context.Context, spec runner.ProcessSpec) error {
	r.started = true
	return r.Runner.Start(ctx, spec)
}

func (r *failReadRunner) Read(ctx context.Context, id string, offset int64) (runner.Output, error) {
	if r.reads == 0 {
		r.reads++
		return runner.Output{}, errors.New("injected read failure")
	}
	return r.Runner.Read(ctx, id, offset)
}

func (r *failReadRunner) Signal(ctx context.Context, id string, signal runner.Signal) error {
	r.signals = append(r.signals, signal)
	return r.Runner.Signal(ctx, id, signal)
}

func TestRunKillsSubprocessWhenCollectFails(t *testing.T) {
	t.Setenv("DALGONA_SHIM_TEST_PAUSE", "pid:2s")
	env, _ := searchTestEnv(t)
	inner := env.Runner
	wrapped := &failReadRunner{Runner: inner}
	env.Runner = wrapped
	t.Cleanup(func() { waitRunExit(t, inner, "search-kill") })
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
	state := waitRunExit(t, inner, "search-kill")
	if state.Exit == nil {
		t.Fatal("killed run has no exit record")
	}
	want := -1
	if runtime.GOOS == "windows" {
		want = 137
	}
	if state.Exit.Code != want {
		t.Fatalf("killed run exit code %d, want %d", state.Exit.Code, want)
	}
}

func waitRunExit(t *testing.T, run runner.Runner, id string) runner.Output {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for {
		state, readErr := run.Read(context.Background(), id, 0)
		if readErr == nil && state.Exit != nil {
			return state
		}
		if errors.Is(readErr, runner.ErrProcessNotFound) {
			return runner.Output{}
		}
		if time.Now().After(deadline) {
			t.Fatalf("subprocess %s still running after 30s: last read error: %v", id, readErr)
		}
		time.Sleep(10 * time.Millisecond)
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
