package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	"net"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

type shutdownHandleCounter struct {
	mu     sync.Mutex
	opened int
	closed int
}

type shutdownCountingRunner struct {
	runner.Runner
	counter *shutdownHandleCounter
}

func (c *shutdownCountingRunner) OpenWorkspace(ctx context.Context, cwd string) (runner.WorkspaceHandle, error) {
	opener, ok := c.Runner.(runner.WorkspaceOpener)
	if !ok {
		return nil, runner.ErrWorkspaceUnsupported
	}
	session, err := opener.OpenWorkspace(ctx, cwd)
	if err != nil {
		return nil, err
	}
	c.counter.mu.Lock()
	c.counter.opened++
	c.counter.mu.Unlock()
	return &shutdownCountingHandle{WorkspaceHandle: session, counter: c.counter}, nil
}

type shutdownCountingHandle struct {
	runner.WorkspaceHandle
	counter *shutdownHandleCounter
}

func (h *shutdownCountingHandle) Close() error {
	h.counter.mu.Lock()
	h.counter.closed++
	h.counter.mu.Unlock()
	return h.WorkspaceHandle.Close()
}

func TestWorkspaceShutdownClosesOpenHandles(t *testing.T) {
	r := newRig(t, "")
	counter := &shutdownHandleCounter{}
	base := r.cfg.Runner
	r.cfg.Runner = func(ctx context.Context, id, name string) (runner.Runner, error) {
		run, err := base(ctx, id, name)
		if err != nil {
			return nil, err
		}
		return &shutdownCountingRunner{Runner: run, counter: counter}, nil
	}
	r.openHost()
	threadID := startWorkspaceThread(t, r, r.cwd)
	serverEnd, clientEnd := net.Pipe()
	w := &workspaceWindow{t: t, decisions: make(chan string, 64), asked: make(chan jsontext.Value, 64), changes: make(chan workspaceChanged, 256)}
	served := make(chan error, 1)
	go func() { served <- ServeWith(t.Context(), serverEnd, r.host, ServeOptions{}) }()
	w.conn = rpc.Dial(t.Context(), clientEnd, w)
	opened := w.open(threadID, "read")
	if opened.WorkspaceID == "" {
		t.Fatal("workspace open returned an empty workspace id")
	}
	if err := w.conn.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-served:
		if err != nil {
			t.Fatalf("ServeWith returned %v", err)
		}
	case <-time.After(waitLimit):
		t.Fatal("server did not stop after the window closed")
	}
	counter.mu.Lock()
	defer counter.mu.Unlock()
	if counter.opened != 1 {
		t.Fatalf("opened %d workspace handles, want 1", counter.opened)
	}
	if counter.closed != counter.opened {
		t.Fatalf("shutdown left %d of %d workspace handles open", counter.opened-counter.closed, counter.opened)
	}
}
