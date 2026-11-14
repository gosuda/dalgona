package remote

import (
	"context"
	"errors"
	"io/fs"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
)

type startFailRunner struct {
	signals []runner.Signal
}

func (f *startFailRunner) Start(context.Context, runner.ProcessSpec) error {
	return errors.New("wire died before reply")
}

func (f *startFailRunner) Read(context.Context, string, int64) (runner.Output, error) {
	return runner.Output{Exit: &runner.Exit{Code: -1}}, nil
}

func (f *startFailRunner) Signal(_ context.Context, _ string, s runner.Signal) error {
	f.signals = append(f.signals, s)
	return nil
}

func (f *startFailRunner) ReadFile(context.Context, string) ([]byte, error) {
	return nil, errors.New("unused")
}

func (f *startFailRunner) WriteFile(context.Context, string, []byte, fs.FileMode) error {
	return errors.New("unused")
}

func (f *startFailRunner) Stat(context.Context, string) (runner.FileInfo, error) {
	return runner.FileInfo{}, errors.New("unused")
}

func (f *startFailRunner) ReadDir(context.Context, string) ([]runner.FileInfo, error) {
	return nil, errors.New("unused")
}

func (f *startFailRunner) Remove(context.Context, string) error { return errors.New("unused") }
func (f *startFailRunner) Close() error                         { return nil }

func TestBoxRun_cleansUpAfterStartError(t *testing.T) {
	f := &startFailRunner{}
	if _, _, err := boxRun(t.Context(), f, "sh", "-c", "true"); err == nil {
		t.Fatal("boxRun error = nil, want start error")
	}
	if slices.Contains(f.signals, runner.SignalKill) {
		return
	}
	t.Fatalf("signals = %v, want cleanup through KILL after Start error", f.signals)
}
