package runner

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"testing"
	"time"
)

type stopFake struct {
	read    Output
	readErr error
	sigErr  error
	signals []Signal
}

func (f *stopFake) Start(context.Context, ProcessSpec) error { return nil }

func (f *stopFake) Read(context.Context, string, int64) (Output, error) {
	return f.read, f.readErr
}

func (f *stopFake) Signal(_ context.Context, _ string, s Signal) error {
	f.signals = append(f.signals, s)
	return f.sigErr
}

func (f *stopFake) ReadFile(context.Context, string) ([]byte, error) {
	return nil, errors.New("unused")
}

func (f *stopFake) WriteFile(context.Context, string, []byte, fs.FileMode) error {
	return errors.New("unused")
}

func (f *stopFake) Stat(context.Context, string) (FileInfo, error) {
	return FileInfo{}, errors.New("unused")
}

func (f *stopFake) ReadDir(context.Context, string) ([]FileInfo, error) {
	return nil, errors.New("unused")
}

func (f *stopFake) Remove(context.Context, string) error { return errors.New("unused") }
func (f *stopFake) Close() error                         { return nil }

type deadStopRunner struct {
	*stopFake
	fresh Runner
}

func (d *deadStopRunner) Redial(context.Context) (Runner, error) { return d.fresh, nil }

func TestStopProcessWithGrace_signalsOverFreshConnectionAfterStreamLoss(t *testing.T) {
	dead := &deadStopRunner{
		stopFake: &stopFake{
			readErr: errRemoteClosed,
			sigErr:  fmt.Errorf("signal on closed remote: %w", errRemoteClosed),
		},
		fresh: &stopFake{read: Output{Exit: &Exit{Code: -1, Signal: "SIGTERM"}}},
	}
	StopProcessWithGrace(dead, "p1", 50*time.Millisecond)
	fresh := dead.fresh.(*stopFake)
	if len(fresh.signals) < 2 || fresh.signals[0] != SignalTerm || fresh.signals[len(fresh.signals)-1] != SignalKill {
		t.Fatalf("fresh signals = %v, want TERM then KILL over the reconnected runner", fresh.signals)
	}
}
