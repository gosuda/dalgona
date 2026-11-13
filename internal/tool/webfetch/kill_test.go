package webfetch

import (
	"context"
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
)

var errKillTestRead = errors.New("webfetch test: injected read failure")

type failReadRemoteRunner struct {
	runner.Runner
	started   bool
	signals   []runner.Signal
	remaining int
	killed    chan struct{}
}

func (r *failReadRemoteRunner) Start(ctx context.Context, spec runner.ProcessSpec) error {
	r.started = true
	return r.Runner.Start(ctx, spec)
}

func (r *failReadRemoteRunner) Read(ctx context.Context, id string, offset int64) (runner.Output, error) {
	if r.remaining > 0 {
		r.remaining--
		return runner.Output{}, errKillTestRead
	}
	return r.Runner.Read(ctx, id, offset)
}

func (r *failReadRemoteRunner) Signal(ctx context.Context, id string, signal runner.Signal) error {
	r.signals = append(r.signals, signal)
	err := r.Runner.Signal(ctx, id, signal)
	if signal == runner.SignalKill && r.killed != nil {
		close(r.killed)
	}
	return err
}

func TestRemoteKillWaitsForHelperExit(t *testing.T) {
	gate := listenShimGate(t)
	t.Setenv("DALGONA_SHIM_TEST_PAUSE", "pid:"+gate.path)
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	wrapped := &failReadRemoteRunner{Runner: local, remaining: 1, killed: make(chan struct{})}
	env := tool.Env{Runner: wrapped, Cwd: t.TempDir()}
	t.Cleanup(func() { waitFetchExit(t, local, "fetch-kill") })
	call := tool.Call{ID: "fetch-kill", Args: []byte(`{"url":"https://example.test/doc","max_bytes":4096}`)}
	finished := make(chan error, 1)
	go func() {
		_, callErr := New().Run(t.Context(), call, env)
		finished <- callErr
	}()
	parked := gate.awaitShim(t)
	awaitKill(t, wrapped.killed)
	_ = parked.Close()
	runErr := awaitRunError(t, finished)
	if runErr == nil {
		t.Fatal("collect failure returned no error")
	}
	if !errors.Is(runErr, errKillTestRead) {
		t.Fatalf("error lost the read cause: %v", runErr)
	}
	if !wrapped.started {
		t.Fatal("subprocess was not started")
	}
	if !slices.Contains(wrapped.signals, runner.SignalKill) {
		t.Fatalf("kill signal missing, got %v", wrapped.signals)
	}
	state := waitFetchExit(t, local, "fetch-kill")
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

func TestRemoteExitWaitReportsTimeout(t *testing.T) {
	steady := &remoteRunner{output: runner.Output{Alive: true}}
	wrapped := &failReadRemoteRunner{Runner: steady, remaining: 1}
	env := tool.Env{Runner: wrapped, Cwd: t.TempDir()}
	call := tool.Call{ID: "fetch-timeout", Args: []byte(`{"url":"https://example.test/doc","max_bytes":4096}`)}
	_, runErr := New().Run(t.Context(), call, env)
	if !errors.Is(runErr, errKillTestRead) {
		t.Fatalf("error lost the read cause: %v", runErr)
	}
	if !errors.Is(runErr, runner.ErrExitTimeout) {
		t.Fatalf("error lost the exit timeout: %v", runErr)
	}
	if !slices.Contains(wrapped.signals, runner.SignalKill) {
		t.Fatalf("kill signal missing, got %v", wrapped.signals)
	}
}

type cancelOnSignalRunner struct {
	*failReadRemoteRunner
	cancel context.CancelCauseFunc
}

func (r *cancelOnSignalRunner) Signal(ctx context.Context, id string, signal runner.Signal) error {
	r.cancel(context.Canceled)
	return r.failReadRemoteRunner.Signal(ctx, id, signal)
}

func TestRemoteExitWaitEndsWithTheCallerContext(t *testing.T) {
	steady := &remoteRunner{output: runner.Output{Alive: true}}
	ctx, cancel := context.WithCancelCause(t.Context())
	defer cancel(nil)
	wrapped := &cancelOnSignalRunner{failReadRemoteRunner: &failReadRemoteRunner{Runner: steady, remaining: 1}, cancel: cancel}
	env := tool.Env{Runner: wrapped, Cwd: t.TempDir()}
	call := tool.Call{ID: "fetch-cancel", Args: []byte(`{"url":"https://example.test/doc","max_bytes":4096}`)}
	_, runErr := New().Run(ctx, call, env)
	if !errors.Is(runErr, errKillTestRead) {
		t.Fatalf("error lost the read cause: %v", runErr)
	}
	if !errors.Is(runErr, context.Canceled) {
		t.Fatalf("exit wait ignored the caller context: %v", runErr)
	}
}

func waitFetchExit(t *testing.T, run runner.Runner, id string) runner.Output {
	t.Helper()
	deadline := time.Now().Add(60 * time.Second)
	for {
		state, readErr := run.Read(context.Background(), id, 0)
		if readErr == nil && state.Exit != nil {
			return state
		}
		if errors.Is(readErr, runner.ErrProcessNotFound) {
			return runner.Output{}
		}
		if time.Now().After(deadline) {
			t.Fatalf("subprocess %s still running after 60s: last read error: %v", id, readErr)
		}
		time.Sleep(25 * time.Millisecond)
	}
}

type shimGate struct {
	path     string
	listener *net.UnixListener
}

func listenShimGate(t *testing.T) shimGate {
	t.Helper()
	dir, err := os.MkdirTemp("", "dgw")
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
