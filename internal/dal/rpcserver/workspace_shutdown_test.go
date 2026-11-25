package rpcserver

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"io"
	"log/slog"
	"net"
	"sync"
	"sync/atomic"
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

type blockingStageRunner struct {
	runner.Runner
	started     chan struct{}
	release     chan struct{}
	finished    chan struct{}
	released    *atomic.Bool
	sawReleased atomic.Bool
	mu          sync.Mutex
	closes      int
}

func (c *blockingStageRunner) OpenWorkspace(ctx context.Context, cwd string) (runner.WorkspaceHandle, error) {
	opener, ok := c.Runner.(runner.WorkspaceOpener)
	if !ok {
		return nil, runner.ErrWorkspaceUnsupported
	}
	session, err := opener.OpenWorkspace(ctx, cwd)
	if err != nil {
		return nil, err
	}
	return &blockingStageHandle{WorkspaceHandle: session, runner: c}, nil
}

type blockingStageHandle struct {
	runner.WorkspaceHandle
	runner *blockingStageRunner
	once   sync.Once
}

func (h *blockingStageHandle) Workspace(ctx context.Context, req runner.WorkspaceRequest) (runner.WorkspaceResult, error) {
	if req.Op == runner.WorkspaceStage {
		h.once.Do(func() { close(h.runner.started) })
		<-h.runner.release
		if released := h.runner.released; released != nil {
			released.Store(true)
		}
	}
	return h.WorkspaceHandle.Workspace(ctx, req)
}

func (h *blockingStageHandle) Close() error {
	if released := h.runner.released; released != nil && released.Load() {
		h.runner.sawReleased.Store(true)
	}
	h.runner.mu.Lock()
	h.runner.closes++
	first := h.runner.closes == 1
	h.runner.mu.Unlock()
	if first {
		close(h.runner.finished)
	}
	return h.WorkspaceHandle.Close()
}

func joinUploadCommit(t *testing.T, commitDone <-chan error) {
	t.Helper()
	commitErr := <-commitDone
	if commitErr != nil && !commitInterruptedByShutdown(commitErr) {
		t.Fatalf("workspace commit failed: %v", commitErr)
	}
}

func commitInterruptedByShutdown(err error) bool {
	if errors.Is(err, io.ErrClosedPipe) {
		return true
	}
	data, ok := rpc.ErrorDataOf(err)
	return ok && (data.Kind == rpc.ErrorUnknownOutcome || data.Kind == rpc.ErrorCancelled)
}

type blockingUploadStage struct {
	started  chan struct{}
	release  chan struct{}
	finished chan struct{}
	blocker  *blockingStageRunner
}

func armBlockingUploadStage(t *testing.T, r *rig, released *atomic.Bool) *blockingUploadStage {
	t.Helper()
	stage := &blockingUploadStage{started: make(chan struct{}), release: make(chan struct{}), finished: make(chan struct{})}
	base := r.cfg.Runner
	r.cfg.Runner = func(ctx context.Context, id, name string) (runner.Runner, error) {
		run, runErr := base(ctx, id, name)
		if runErr != nil {
			return nil, runErr
		}
		stage.blocker = &blockingStageRunner{Runner: run, started: stage.started, release: stage.release, finished: stage.finished, released: released}
		return stage.blocker, nil
	}
	return stage
}

func serveQuietWorkspaceWindow(t *testing.T, r *rig, ctx context.Context) (*workspaceWindow, chan error) {
	t.Helper()
	serverEnd, clientEnd := net.Pipe()
	w := &workspaceWindow{t: t, decisions: make(chan string, 64), asked: make(chan jsontext.Value, 64), changes: make(chan workspaceChanged, 256)}
	served := make(chan error, 1)
	quiet := slog.New(slog.DiscardHandler)
	go func() { served <- ServeWith(ctx, serverEnd, r.host, ServeOptions{Logger: quiet}) }()
	w.conn = rpc.Dial(t.Context(), clientEnd, w)
	return w, served
}

func startHeldUploadCommit(t *testing.T, w *workspaceWindow, workspaceID, path string, data []byte) <-chan error {
	t.Helper()
	sum := sha256.Sum256(data)
	var begun workspaceUploadBegun
	w.must("dalgona/workspace/write", map[string]any{"stage": "begin", "workspaceId": workspaceID, "path": path, "expectedRevision": runner.WorkspaceAbsent, "total": rpc.FormatDecimal(uint64(len(data))), "digest": hex.EncodeToString(sum[:]), "operationId": rpc.NewOperationID()}, &begun)
	w.must("dalgona/workspace/write", map[string]any{"stage": "chunk", "uploadId": begun.UploadID, "offset": "0", "dataBase64": base64.StdEncoding.EncodeToString(data)}, nil)
	commitDone := make(chan error, 1)
	uploadID := begun.UploadID
	go func() {
		var raw jsontext.Value
		commitDone <- w.conn.Call(t.Context(), "dalgona/workspace/write", map[string]any{"stage": "commit", "uploadId": uploadID, "operationId": rpc.NewOperationID()}, &raw)
	}()
	return commitDone
}

func requireShutdownWaitingForUpload(t *testing.T, served <-chan error, commitDone <-chan error, release chan struct{}, w *workspaceWindow) {
	t.Helper()
	select {
	case serveErr := <-served:
		close(release)
		joinUploadCommit(t, commitDone)
		_ = w.conn.Close()
		t.Fatalf("shutdown returned while the upload still held its workspace (served=%v)", serveErr)
	case <-time.After(time.Second):
	}
}

func awaitReleaseDrivenShutdown(t *testing.T, served <-chan error) {
	t.Helper()
	select {
	case serveErr := <-served:
		if serveErr != nil {
			t.Fatalf("ServeWith returned %v", serveErr)
		}
	case <-time.After(waitLimit):
		t.Fatal("server did not stop after upload released")
	}
}

func awaitWorkspaceRootClosed(t *testing.T, finished <-chan struct{}) {
	t.Helper()
	select {
	case <-finished:
	case <-time.After(waitLimit):
		t.Fatal("workspace root not closed after upload released")
	}
}

func requireCloseAfterUploadReleased(t *testing.T, blocker *blockingStageRunner) {
	t.Helper()
	blocker.mu.Lock()
	closes := blocker.closes
	blocker.mu.Unlock()
	if closes != 1 {
		t.Fatalf("workspace closed %d times, want 1", closes)
	}
	if !blocker.sawReleased.Load() {
		t.Fatal("workspace root closed before the upload released it")
	}
}

func TestWorkspaceShutdownDefersCloseUntilUploadReleased(t *testing.T) {
	r := newRig(t, "")
	stage := armBlockingUploadStage(t, r, &atomic.Bool{})
	r.openHost()
	threadID := startWorkspaceThread(t, r, r.cwd)
	serveCtx, cancel := context.WithCancel(t.Context())
	defer cancel()
	w, served := serveQuietWorkspaceWindow(t, r, serveCtx)
	opened := w.open(threadID, "read", "write")
	commitDone := startHeldUploadCommit(t, w, opened.WorkspaceID, "held.txt", []byte("held"))
	<-stage.started
	cancel()
	requireShutdownWaitingForUpload(t, served, commitDone, stage.release, w)
	close(stage.release)
	awaitReleaseDrivenShutdown(t, served)
	joinUploadCommit(t, commitDone)
	awaitWorkspaceRootClosed(t, stage.finished)
	requireCloseAfterUploadReleased(t, stage.blocker)
	_ = w.conn.Close()
}

func TestWorkspaceShutdownClosesWhenUploadNeverReleases(t *testing.T) {
	r := newRig(t, "")
	started := make(chan struct{})
	release := make(chan struct{})
	finished := make(chan struct{})
	var blocker *blockingStageRunner
	base := r.cfg.Runner
	r.cfg.Runner = func(ctx context.Context, id, name string) (runner.Runner, error) {
		run, err := base(ctx, id, name)
		if err != nil {
			return nil, err
		}
		blocker = &blockingStageRunner{Runner: run, started: started, release: release, finished: finished}
		return blocker, nil
	}
	r.openHost()
	threadID := startWorkspaceThread(t, r, r.cwd)
	serverEnd, clientEnd := net.Pipe()
	w := &workspaceWindow{t: t, decisions: make(chan string, 64), asked: make(chan jsontext.Value, 64), changes: make(chan workspaceChanged, 256)}
	served := make(chan error, 1)
	quiet := slog.New(slog.DiscardHandler)
	go func() { served <- ServeWith(t.Context(), serverEnd, r.host, ServeOptions{Logger: quiet}) }()
	w.conn = rpc.Dial(t.Context(), clientEnd, w)
	opened := w.open(threadID, "read", "write")
	data := []byte("stuck")
	sum := sha256.Sum256(data)
	var begun workspaceUploadBegun
	w.must("dalgona/workspace/write", map[string]any{"stage": "begin", "workspaceId": opened.WorkspaceID, "path": "stuck.txt", "expectedRevision": runner.WorkspaceAbsent, "total": rpc.FormatDecimal(uint64(len(data))), "digest": hex.EncodeToString(sum[:]), "operationId": rpc.NewOperationID()}, &begun)
	w.must("dalgona/workspace/write", map[string]any{"stage": "chunk", "uploadId": begun.UploadID, "offset": "0", "dataBase64": base64.StdEncoding.EncodeToString(data)}, nil)
	commitDone := make(chan error, 1)
	go func() {
		var raw jsontext.Value
		commitDone <- w.conn.Call(t.Context(), "dalgona/workspace/write", map[string]any{"stage": "commit", "uploadId": begun.UploadID, "operationId": rpc.NewOperationID()}, &raw)
	}()
	<-started
	if err := w.conn.Close(); err != nil {
		t.Fatal(err)
	}
	select {
	case err := <-served:
		if err != nil {
			t.Fatalf("ServeWith returned %v", err)
		}
	case <-time.After(90 * time.Second):
		t.Fatal("server did not stop while upload held")
	}
	select {
	case <-finished:
	default:
		t.Fatal("workspace root not closed before ServeWith returned")
	}
	close(release)
	joinUploadCommit(t, commitDone)
	blocker.mu.Lock()
	closes := blocker.closes
	blocker.mu.Unlock()
	if closes != 1 {
		t.Fatalf("workspace closed %d times, want 1", closes)
	}
}
