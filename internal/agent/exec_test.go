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

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
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

func TestExecReadsLiveSettingsHome(t *testing.T) {
	h := newHarness(t)
	moved := t.TempDir()
	live := h.host.Config()
	live.Home = moved
	h.host.settingsMu.Lock()
	h.host.live = live
	h.host.settingsMu.Unlock()

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
