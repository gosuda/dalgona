package runner

import (
	"context"
	"errors"
	"io/fs"
	"testing"
	"time"
)

type killRecorder struct {
	started ProcessSpec
	signals []Signal
	killed  bool
}

func (k *killRecorder) Start(_ context.Context, spec ProcessSpec) error {
	k.started = spec
	return nil
}

func (k *killRecorder) Read(_ context.Context, _ string, _ int64) (Output, error) {
	if k.killed {
		return Output{Exit: &Exit{Code: -1, Signal: "SIGKILL"}}, nil
	}
	return Output{Data: []byte("running\n")}, nil
}

func (k *killRecorder) Signal(_ context.Context, _ string, s Signal) error {
	k.signals = append(k.signals, s)
	if s == SignalKill {
		k.killed = true
	}
	return nil
}

func (k *killRecorder) ReadFile(context.Context, string) ([]byte, error) {
	return nil, errors.New("unused")
}

func (k *killRecorder) WriteFile(context.Context, string, []byte, fs.FileMode) error {
	return errors.New("unused")
}

func (k *killRecorder) Stat(context.Context, string) (FileInfo, error) {
	return FileInfo{}, errors.New("unused")
}

func (k *killRecorder) ReadDir(context.Context, string) ([]FileInfo, error) {
	return nil, errors.New("unused")
}
func (k *killRecorder) Remove(context.Context, string) error { return errors.New("unused") }
func (k *killRecorder) Close() error                         { return nil }

func TestRunRemote_signalsProcessOnCancel(t *testing.T) {
	runnerKillGrace = 50 * time.Millisecond
	rec := &killRecorder{}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	defer cancel()
	_, err := runRemote(ctx, rec, []string{"sleep", "60"})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("runRemote error = %v, want context.Canceled", err)
	}
	if len(rec.signals) < 2 || rec.signals[0] != SignalTerm || rec.signals[len(rec.signals)-1] != SignalKill {
		t.Fatalf("signals = %v, want TERM then KILL", rec.signals)
	}
}

func TestWaitExit_signalsProcessOnReadError(t *testing.T) {
	runnerKillGrace = 50 * time.Millisecond
	rec := &killRecorder{}
	_, _, err := waitExit(t.Context(), &readErrorRunner{rec}, "p1")
	if err == nil {
		t.Fatal("waitExit error = nil, want read error")
	}
	if len(rec.signals) < 2 || rec.signals[0] != SignalTerm || rec.signals[len(rec.signals)-1] != SignalKill {
		t.Fatalf("signals = %v, want TERM then KILL", rec.signals)
	}
}

type readErrorRunner struct {
	rec *killRecorder
}

func (r *readErrorRunner) Start(ctx context.Context, spec ProcessSpec) error {
	return r.rec.Start(ctx, spec)
}

func (r *readErrorRunner) Read(context.Context, string, int64) (Output, error) {
	return Output{}, errors.New("stream lost")
}

func (r *readErrorRunner) Signal(ctx context.Context, id string, s Signal) error {
	return r.rec.Signal(ctx, id, s)
}

func (r *readErrorRunner) ReadFile(context.Context, string) ([]byte, error) {
	return nil, errors.New("unused")
}

func (r *readErrorRunner) WriteFile(context.Context, string, []byte, fs.FileMode) error {
	return errors.New("unused")
}

func (r *readErrorRunner) Stat(context.Context, string) (FileInfo, error) {
	return FileInfo{}, errors.New("unused")
}

func (r *readErrorRunner) ReadDir(context.Context, string) ([]FileInfo, error) {
	return nil, errors.New("unused")
}
func (r *readErrorRunner) Remove(context.Context, string) error { return errors.New("unused") }
func (r *readErrorRunner) Close() error                         { return nil }

func TestWaitExit_returnsOnExitWithoutSignal(t *testing.T) {
	rec := &killRecorder{}
	rec.killed = true
	_, code, err := waitExit(t.Context(), rec, "p1")
	if err != nil || code != -1 {
		t.Fatalf("waitExit = (%d, %v), want (-1, nil)", code, err)
	}
	if len(rec.signals) != 0 {
		t.Fatalf("signals = %v, want none on normal exit", rec.signals)
	}
}

type stopPollFake struct {
	signals []Signal
	reads   int
	offsets []int64
	bytes   int64
}

func (f *stopPollFake) Start(context.Context, ProcessSpec) error { return nil }

func (f *stopPollFake) Read(_ context.Context, _ string, off int64) (Output, error) {
	f.reads++
	f.offsets = append(f.offsets, off)
	data := []byte("running\n")
	f.bytes += int64(len(data))
	return Output{Data: data, Next: off + int64(len(data))}, nil
}

func (f *stopPollFake) Signal(_ context.Context, _ string, s Signal) error {
	f.signals = append(f.signals, s)
	return nil
}

func (f *stopPollFake) ReadFile(context.Context, string) ([]byte, error) {
	return nil, errors.New("unused")
}

func (f *stopPollFake) WriteFile(context.Context, string, []byte, fs.FileMode) error {
	return errors.New("unused")
}

func (f *stopPollFake) Stat(context.Context, string) (FileInfo, error) {
	return FileInfo{}, errors.New("unused")
}

func (f *stopPollFake) ReadDir(context.Context, string) ([]FileInfo, error) {
	return nil, errors.New("unused")
}

func (f *stopPollFake) Remove(context.Context, string) error { return errors.New("unused") }
func (f *stopPollFake) Close() error                         { return nil }

func TestStopRunnerProcess_pollsWithOffset(t *testing.T) {
	const grace = 200 * time.Millisecond
	old := runnerKillGrace
	runnerKillGrace = grace
	defer func() { runnerKillGrace = old }()
	f := &stopPollFake{}
	stopRunnerProcess(f, "p1")
	if len(f.signals) < 2 || f.signals[0] != SignalTerm || f.signals[len(f.signals)-1] != SignalKill {
		t.Fatalf("signals = %v, want TERM then KILL", f.signals)
	}
	wantMax := int(grace/pollInterval) + 5
	if f.reads > wantMax {
		t.Fatalf("reads = %d, want <= %d (grace/interval); unbounded burst", f.reads, wantMax)
	}
	var maxOff int64
	for _, off := range f.offsets {
		maxOff = max(maxOff, off)
	}
	if maxOff <= 0 {
		t.Fatalf("max offset = %d, want > 0 (offset reads)", maxOff)
	}
	if f.bytes != int64(f.reads*len("running\n")) {
		t.Fatalf("bytes = %d, want %d", f.bytes, f.reads*len("running\n"))
	}
}
