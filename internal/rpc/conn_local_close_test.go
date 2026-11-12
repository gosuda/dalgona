package rpc

import (
	"bytes"
	"log/slog"
	"net"
	"strings"
	"sync"
	"testing"
)

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

func TestLocalCloseIsNotLoggedAsReadFailure(t *testing.T) {
	var log bytes.Buffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&log, nil)))
	t.Cleanup(func() { slog.SetDefault(previous) })

	rwc := &readAfterLocalClose{closed: make(chan struct{})}
	NewConn(t.Context(), rwc, nil)
	<-rwc.closed
	if out := log.String(); strings.Contains(out, "rpc: read failure") {
		t.Fatalf("a connection the client closed itself was logged as a read failure: %s", out)
	}
}
