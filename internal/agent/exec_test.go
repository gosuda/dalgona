package agent

import (
	"context"
	"errors"
	"slices"
	"sync"
	"testing"

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
