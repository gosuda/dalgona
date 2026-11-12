package search

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"net"
	"os"
	"path/filepath"
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
	killed  chan struct{}
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
	err := r.Runner.Signal(ctx, id, signal)
	if signal == runner.SignalKill && r.killed != nil {
		close(r.killed)
	}
	return err
}

func TestRunKillsSubprocessWhenCollectFails(t *testing.T) {
	gate := listenShimGate(t)
	t.Setenv("DALGONA_SHIM_TEST_PAUSE", "pid:"+gate.path)
	env, _ := searchTestEnv(t)
	inner := env.Runner
	wrapped := &failReadRunner{Runner: inner, killed: make(chan struct{})}
	env.Runner = wrapped
	t.Cleanup(func() { waitRunExit(t, inner, "search-kill") })
	finished := make(chan error, 1)
	go func() {
		_, err := Tool{}.Run(t.Context(), tool.Call{ID: "search-kill", Args: jsontext.Value(`{"mode":"grep","pattern":"x"}`)}, env)
		finished <- err
	}()
	parked := gate.awaitShim(t)
	awaitKill(t, wrapped.killed)
	_ = parked.Close()
	err := awaitRunError(t, finished)
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

type shimGate struct {
	path     string
	listener *net.UnixListener
}

func listenShimGate(t *testing.T) shimGate {
	t.Helper()
	dir, err := os.MkdirTemp("", "dgs")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	path := filepath.Join(dir, "s")
	listener, err := net.ListenUnix("unix", &net.UnixAddr{Name: path, Net: "unix"})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	return shimGate{path: path, listener: listener}
}

func (g shimGate) awaitShim(t *testing.T) net.Conn {
	t.Helper()
	if err := g.listener.SetDeadline(time.Now().Add(15 * time.Second)); err != nil {
		t.Fatal(err)
	}
	conn, err := g.listener.Accept()
	if err != nil {
		t.Fatalf("shim never reached the pid pause: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

func awaitKill(t *testing.T, killed <-chan struct{}) {
	t.Helper()
	select {
	case <-killed:
	case <-time.After(15 * time.Second):
		t.Fatal("collect failure never requested a kill")
	}
}

func awaitRunError(t *testing.T, finished <-chan error) error {
	t.Helper()
	select {
	case err := <-finished:
		return err
	case <-time.After(15 * time.Second):
		t.Fatal("run did not return after the kill")
		return nil
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
