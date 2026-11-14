package remote

import (
	"context"
	"errors"
	"io/fs"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
)

type termFailRunner struct {
	signals []runner.Signal
}

func (f *termFailRunner) Start(context.Context, runner.ProcessSpec) error { return nil }

func (f *termFailRunner) Read(context.Context, string, int64) (runner.Output, error) {
	return runner.Output{Exit: &runner.Exit{Code: -1}}, nil
}

func (f *termFailRunner) Signal(_ context.Context, _ string, s runner.Signal) error {
	f.signals = append(f.signals, s)
	if s == runner.SignalTerm {
		return errors.New("wire dead")
	}
	return nil
}

func (f *termFailRunner) ReadFile(context.Context, string) ([]byte, error) {
	return nil, errors.New("unused")
}

func (f *termFailRunner) WriteFile(context.Context, string, []byte, fs.FileMode) error {
	return errors.New("unused")
}

func (f *termFailRunner) Stat(context.Context, string) (runner.FileInfo, error) {
	return runner.FileInfo{}, errors.New("unused")
}

func (f *termFailRunner) ReadDir(context.Context, string) ([]runner.FileInfo, error) {
	return nil, errors.New("unused")
}

func (f *termFailRunner) Remove(context.Context, string) error { return errors.New("unused") }
func (f *termFailRunner) Close() error                         { return nil }

func TestStopProcess_killsAfterTermFailure(t *testing.T) {
	f := &termFailRunner{}
	stopProcess(f, "p1")
	if slices.Contains(f.signals, runner.SignalKill) {
		return
	}
	t.Fatalf("signals = %v, want KILL despite TERM failure", f.signals)
}
