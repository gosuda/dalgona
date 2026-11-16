package rpc

import (
	"bytes"
	"errors"
	"io"
	"log/slog"
	"net"
	"strings"
	"sync"
	"testing"
	"time"
)

type safeBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *safeBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *safeBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

type readAfterLocalClose struct {
	closed    chan struct{}
	closeOnce sync.Once
}

func (r *readAfterLocalClose) Read([]byte) (int, error) {
	return 0, &net.OpError{Op: "read", Net: "unix", Err: net.ErrClosed}
}

func (r *readAfterLocalClose) Write(p []byte) (int, error) { return len(p), nil }

func (r *readAfterLocalClose) Close() error {
	r.closeOnce.Do(func() { close(r.closed) })
	return nil
}

type errorOnRead struct {
	err       error
	closed    chan struct{}
	closeOnce sync.Once
}

func (r *errorOnRead) Read([]byte) (int, error) {
	return 0, r.err
}

func (r *errorOnRead) Write(p []byte) (int, error) { return len(p), nil }

func (r *errorOnRead) Close() error {
	r.closeOnce.Do(func() { close(r.closed) })
	return nil
}

func TestLocalCloseIsNotLoggedAsReadFailure(t *testing.T) {
	var log safeBuffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&log, nil)))
	t.Cleanup(func() { slog.SetDefault(previous) })

	rwc := &readAfterLocalClose{closed: make(chan struct{})}
	NewConn(t.Context(), rwc, nil)
	select {
	case <-rwc.closed:
	case <-time.After(waitLimit):
		t.Fatal("connection did not close within deadline")
	}
	if out := log.String(); strings.Contains(out, "rpc: read failure") {
		t.Fatalf("a connection the client closed itself was logged as a read failure: %s", out)
	}
}

func TestClosedPipeIsNotLoggedAsReadFailure(t *testing.T) {
	var log safeBuffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&log, nil)))
	t.Cleanup(func() { slog.SetDefault(previous) })

	rwc := &errorOnRead{err: io.ErrClosedPipe, closed: make(chan struct{})}
	NewConn(t.Context(), rwc, nil)
	select {
	case <-rwc.closed:
	case <-time.After(waitLimit):
		t.Fatal("connection did not close within deadline")
	}
	if out := log.String(); strings.Contains(out, "rpc: read failure") {
		t.Fatalf("closed pipe was logged as a read failure: %s", out)
	}
}

func TestUnexpectedReadFailureIsLogged(t *testing.T) {
	var log safeBuffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&log, nil)))
	t.Cleanup(func() { slog.SetDefault(previous) })

	readErr := errors.New("simulated network read failure")
	rwc := &errorOnRead{err: readErr, closed: make(chan struct{})}
	NewConn(t.Context(), rwc, nil)
	select {
	case <-rwc.closed:
	case <-time.After(waitLimit):
		t.Fatal("connection did not close within deadline")
	}
	out := log.String()
	if !strings.Contains(out, "rpc: read failure") {
		t.Fatalf("expected read failure log, got: %s", out)
	}
	if !strings.Contains(out, readErr.Error()) {
		t.Fatalf("expected log to contain %q, got: %s", readErr.Error(), out)
	}
}
