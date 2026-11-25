package agent

import (
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"sync"
	"testing"
	"testing/synctest"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

type readFailingRunner struct {
	runner.Runner
	mu      sync.Mutex
	signals []runner.Signal
	readErr error
}

func (f *readFailingRunner) Read(context.Context, string, int64) (runner.Output, error) {
	return runner.Output{Alive: true}, f.readErr
}

func (f *readFailingRunner) Signal(_ context.Context, _ string, s runner.Signal) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.signals = append(f.signals, s)
	return nil
}

func TestCollectExecKillsWhenReadFailsWhileAlive(t *testing.T) {
	readErr := errors.New("read failed")
	run := &readFailingRunner{readErr: readErr}
	_, err := collectExec(t.Context(), run, "exec-1", ExecSpec{})
	if !errors.Is(err, readErr) {
		t.Fatalf("collectExec error = %v, want wrapped read error %v", err, readErr)
	}
	run.mu.Lock()
	defer run.mu.Unlock()
	if !slices.Contains(run.signals, runner.SignalKill) {
		t.Fatalf("signals %v after a failed read of a live process, want a KILL", run.signals)
	}
}

type stalledExecRunner struct {
	runner.Runner
	stop       context.Context
	signals    []runner.Signal
	stallReads bool
}

func (r *stalledExecRunner) Read(ctx context.Context, _ string, _ int64) (runner.Output, error) {
	if !r.stallReads {
		return runner.Output{Alive: true}, nil
	}
	return runner.Output{}, r.await(ctx)
}

func (r *stalledExecRunner) Signal(ctx context.Context, _ string, signal runner.Signal) error {
	r.signals = append(r.signals, signal)
	return r.await(ctx)
}

func (r *stalledExecRunner) await(ctx context.Context) error {
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-r.stop.Done():
		return errors.New("the runner never replied")
	}
}

func TestCollectExecBoundsUnresponsiveRunner(t *testing.T) {
	for _, test := range []struct {
		name       string
		stallReads bool
	}{
		{name: "read and signal", stallReads: true},
		{name: "signal only"},
	} {
		t.Run(test.name, func(t *testing.T) {
			synctest.Test(t, func(t *testing.T) {
				stop, release := context.WithTimeout(t.Context(), 20*time.Second)
				defer release()
				ctx, cancel := context.WithTimeout(t.Context(), 6*time.Second)
				defer cancel()
				run := &stalledExecRunner{stop: stop, stallReads: test.stallReads}
				started := time.Now()
				result, err := collectExec(ctx, run, "exec-1", ExecSpec{Timeout: time.Second})
				if elapsed := time.Since(started); err == nil || elapsed > 6*time.Second {
					t.Fatalf("no confirmed exit: result = %+v, err = %v, elapsed %v; want error by the request deadline", result, err, elapsed)
				}
				synctest.Wait()
				if !slices.Equal(run.signals, []runner.Signal{runner.SignalTerm, runner.SignalKill}) {
					t.Fatalf("signals = %v; want TERM then KILL", run.signals)
				}
			})
		})
	}
}

func TestExecReadsLiveSettingsHome(t *testing.T) {
	h := newHarness(t)
	moved := t.TempDir()
	live := h.host.Config()
	live.Home = moved
	h.host.settingsStateMu.Lock()
	h.host.live = live
	h.host.settingsStateMu.Unlock()

	argv := []string{"sh", "-c", "exit 0"}
	if runtime.GOOS == "windows" {
		argv = []string{"cmd", "/C", "exit 0"}
	}
	result, err := h.host.Exec(t.Context(), "live-home", ExecSpec{Argv: argv, Sandbox: config.SandboxOff})
	if err != nil {
		t.Fatal(err)
	}
	if result.ExitCode != 0 {
		t.Fatalf("exit code = %d, want 0", result.ExitCode)
	}
	if _, err := os.Stat(filepath.Join(moved, "exec")); err != nil {
		t.Fatalf("exec runner is not under the live home: %v", err)
	}
	if _, err := os.Stat(filepath.Join(h.home, "exec")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("stale home %s holds an exec runner: %v", h.home, err)
	}
}
