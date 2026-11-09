package remote

import (
	"context"
	"errors"
	"io/fs"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
)

type boxKillRecorder struct {
	signals []runner.Signal
	killed  bool
}

func (k *boxKillRecorder) Start(_ context.Context, _ runner.ProcessSpec) error { return nil }

func (k *boxKillRecorder) Read(_ context.Context, _ string, _ int64) (runner.Output, error) {
	if k.killed {
		return runner.Output{Exit: &runner.Exit{Code: -1, Signal: "SIGKILL"}}, nil
	}
	return runner.Output{Data: []byte("running\n")}, nil
}

func (k *boxKillRecorder) Signal(_ context.Context, _ string, s runner.Signal) error {
	k.signals = append(k.signals, s)
	if s == runner.SignalKill {
		k.killed = true
	}
	return nil
}

func (k *boxKillRecorder) ReadFile(context.Context, string) ([]byte, error) {
	return nil, errors.New("unused")
}

func (k *boxKillRecorder) WriteFile(context.Context, string, []byte, fs.FileMode) error {
	return errors.New("unused")
}

func (k *boxKillRecorder) Stat(context.Context, string) (runner.FileInfo, error) {
	return runner.FileInfo{}, errors.New("unused")
}

func (k *boxKillRecorder) ReadDir(context.Context, string) ([]runner.FileInfo, error) {
	return nil, errors.New("unused")
}
func (k *boxKillRecorder) Remove(context.Context, string) error { return errors.New("unused") }
func (k *boxKillRecorder) Close() error                         { return nil }

func TestBoxRun_signalsProcessOnCancel(t *testing.T) {
	boxKillGrace = 50 * time.Millisecond
	rec := &boxKillRecorder{}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	defer cancel()
	_, _, err := boxRun(ctx, rec, "sh", "-c", "sleep 60")
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("boxRun error = %v, want context.Canceled", err)
	}
	if len(rec.signals) < 2 || rec.signals[0] != runner.SignalTerm || rec.signals[len(rec.signals)-1] != runner.SignalKill {
		t.Fatalf("signals = %v, want TERM then KILL", rec.signals)
	}
}

func TestBoxRun_signalsProcessOnReadError(t *testing.T) {
	boxKillGrace = 50 * time.Millisecond
	rec := &boxKillRecorder{}
	_, _, err := boxRun(t.Context(), &readLossRunner{rec}, "sh", "-c", "sleep 60")
	if err == nil {
		t.Fatal("boxRun error = nil, want read error")
	}
	if len(rec.signals) < 2 || rec.signals[0] != runner.SignalTerm || rec.signals[len(rec.signals)-1] != runner.SignalKill {
		t.Fatalf("signals = %v, want TERM then KILL", rec.signals)
	}
}

type readLossRunner struct {
	rec *boxKillRecorder
}

func (r *readLossRunner) Start(context.Context, runner.ProcessSpec) error { return nil }

func (r *readLossRunner) Read(context.Context, string, int64) (runner.Output, error) {
	return runner.Output{}, errors.New("stream lost")
}

func (r *readLossRunner) Signal(ctx context.Context, id string, s runner.Signal) error {
	return r.rec.Signal(ctx, id, s)
}

func (r *readLossRunner) ReadFile(context.Context, string) ([]byte, error) {
	return nil, errors.New("unused")
}

func (r *readLossRunner) WriteFile(context.Context, string, []byte, fs.FileMode) error {
	return errors.New("unused")
}

func (r *readLossRunner) Stat(context.Context, string) (runner.FileInfo, error) {
	return runner.FileInfo{}, errors.New("unused")
}

func (r *readLossRunner) ReadDir(context.Context, string) ([]runner.FileInfo, error) {
	return nil, errors.New("unused")
}
func (r *readLossRunner) Remove(context.Context, string) error { return errors.New("unused") }
func (r *readLossRunner) Close() error                         { return nil }
