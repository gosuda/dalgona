package runner

import (
	"io"
	"sync"
	"testing"
	"time"
)

type releaseSignalFakeTerminal struct {
	mu      sync.Mutex
	signals []Signal
	done    chan struct{}
	exit    Exit
}

func newReleaseSignalFakeTerminal(exited bool) *releaseSignalFakeTerminal {
	f := &releaseSignalFakeTerminal{done: make(chan struct{})}
	if exited {
		close(f.done)
	}
	return f
}

func (f *releaseSignalFakeTerminal) Read(p []byte) (int, error) {
	return 0, io.EOF
}

func (f *releaseSignalFakeTerminal) Write(p []byte) (int, error) {
	return len(p), nil
}

func (f *releaseSignalFakeTerminal) Resize(columns, rows int) error {
	return nil
}

func (f *releaseSignalFakeTerminal) Signal(signal Signal) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.signals = append(f.signals, signal)
	return nil
}

func (f *releaseSignalFakeTerminal) Done() <-chan struct{} {
	return f.done
}

func (f *releaseSignalFakeTerminal) Exit() Exit {
	<-f.done
	return f.exit
}

func (f *releaseSignalFakeTerminal) Close() error {
	return nil
}

func (f *releaseSignalFakeTerminal) killSignals() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	n := 0
	for _, s := range f.signals {
		if s == SignalKill {
			n++
		}
	}
	return n
}

func serveReleaseSignalFake(t *testing.T, term Terminal) string {
	t.Helper()
	id, err := servedTerminalID()
	if err != nil {
		t.Fatal(err)
	}
	served := &servedTerminal{term: term, writes: make(chan writeRequest, 1), stop: make(chan struct{})}
	served.space = sync.NewCond(&served.mu)
	served.idle = time.AfterFunc(servedTerminalIdle, func() { releaseServedTerminal(id) })
	servedTerminals.mu.Lock()
	servedTerminals.byID[id] = served
	servedTerminals.mu.Unlock()
	t.Cleanup(func() { releaseServedTerminal(id) })
	return id
}

func TestReleaseServedTerminalSkipsKillWhenExited(t *testing.T) {
	term := newReleaseSignalFakeTerminal(true)
	id := serveReleaseSignalFake(t, term)
	releaseServedTerminal(id)
	if got := term.killSignals(); got != 0 {
		t.Fatalf("kill signals after releasing exited terminal = %d, want 0", got)
	}
}

func TestReleaseServedTerminalKillsRunningTerminal(t *testing.T) {
	term := newReleaseSignalFakeTerminal(false)
	id := serveReleaseSignalFake(t, term)
	releaseServedTerminal(id)
	if got := term.killSignals(); got != 1 {
		t.Fatalf("kill signals after releasing running terminal = %d, want 1", got)
	}
}
