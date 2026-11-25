package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	"log/slog"
	"net"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

type terminalShutdownFiles struct {
	closes atomic.Int64
}

func (f *terminalShutdownFiles) Workspace(context.Context, runner.WorkspaceRequest) (runner.WorkspaceResult, error) {
	return runner.WorkspaceResult{}, nil
}

func (f *terminalShutdownFiles) Close() error {
	f.closes.Add(1)
	return nil
}

type terminalShutdownHarness struct {
	host   *TerminalHost
	srv    *server
	files  *terminalShutdownFiles
	term   *terminal
	secret string
}

func newTerminalShutdownHarness(t *testing.T) *terminalShutdownHarness {
	t.Helper()
	h := &terminalShutdownHarness{host: NewTerminalHost(transcriptEngine), srv: &server{}, files: &terminalShutdownFiles{}}
	h.srv.opts = ServeOptions{Terminals: h.host}
	h.srv.workspace.init()
	h.srv.workspace.handles["ws-terminal"] = &workspaceHandle{
		id:     "ws-terminal",
		rights: rightRead | rightTerminal,
		files:  h.files,
		reads:  map[uint64]context.CancelFunc{},
		used:   time.Now(),
	}
	use, acquireErr := h.srv.workspace.acquire(context.Background(), "ws-terminal", rightTerminal, true)
	if acquireErr != nil {
		t.Fatal(acquireErr)
	}
	t.Cleanup(use.release)
	proc := newScriptedTerminal()
	if reserveErr := h.host.reserve(); reserveErr != nil {
		t.Fatal(reserveErr)
	}
	h.term = newTerminal(h.host, terminalIdentity{threadID: "thread", runnerID: "local", bindingRevision: "1", root: "/work"}, proc, 80, 24)
	h.term.mu.Lock()
	_, _, attachErr := h.term.attachLocked(attachRequest{owner: h.srv, workspaceID: "ws-terminal", release: use.release, validate: permitTestTerminal, access: accessWrite}, time.Now())
	h.term.mu.Unlock()
	if attachErr != nil {
		t.Fatal(attachErr)
	}
	h.host.insert(h.term)
	t.Cleanup(h.term.release)
	h.term.mu.Lock()
	h.secret = ""
	if h.term.writer != nil {
		h.secret = h.term.writer.secret
	}
	h.term.mu.Unlock()
	if h.secret == "" {
		t.Fatal("attached terminal holds no writer lease")
	}
	return h
}

func awaitTerminalShutdown(t *testing.T, s *server) {
	t.Helper()
	done := make(chan struct{})
	go func() {
		s.shutdown()
		close(done)
	}()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("shutdown with an attached terminal did not return inside the restart budget")
	}
}

func requireTerminalWorkspaceReleased(t *testing.T, h *terminalShutdownHarness) {
	t.Helper()
	if got := h.files.closes.Load(); got != 1 {
		t.Fatalf("workspace root closed %d times, want 1", got)
	}
	if len(h.srv.workspace.handles) != 0 {
		t.Fatal("shutdown left workspace handles behind")
	}
}

func requireTerminalAttachmentPreserved(t *testing.T, h *terminalShutdownHarness) {
	t.Helper()
	if !h.term.attachedBy(h.srv) {
		t.Fatal("shutdown dropped the attachment instead of only its workspace ownership")
	}
	h.term.mu.Lock()
	kept := h.term.writer != nil && h.term.writer.secret == h.secret
	h.term.mu.Unlock()
	if !kept {
		t.Fatal("shutdown cleared the writer lease instead of preserving it for reconnect")
	}
}

func TestServerShutdownReleasesTerminalWorkspaceOwners(t *testing.T) {
	h := newTerminalShutdownHarness(t)
	awaitTerminalShutdown(t, h.srv)
	requireTerminalWorkspaceReleased(t, h)
	requireTerminalAttachmentPreserved(t, h)
}

type terminalShutdownRunner struct {
	*shutdownCountingRunner
}

func (c *terminalShutdownRunner) OpenTerminal(context.Context, runner.TerminalSpec) (runner.Terminal, error) {
	return newScriptedTerminal(), nil
}

type terminalShutdownAttached struct {
	Attachment struct {
		AttachmentID string `json:"attachmentId"`
		WriterLease  string `json:"writerLease"`
	} `json:"attachment"`
}

func installTerminalShutdownRunner(t *testing.T, r *rig, counter *shutdownHandleCounter) {
	t.Helper()
	base := r.cfg.Runner
	r.cfg.Runner = func(ctx context.Context, id, name string) (runner.Runner, error) {
		run, runErr := base(ctx, id, name)
		if runErr != nil {
			return nil, runErr
		}
		return &terminalShutdownRunner{shutdownCountingRunner: &shutdownCountingRunner{Runner: run, counter: counter}}, nil
	}
}

func dialTerminalShutdownServer(t *testing.T, r *rig, host *TerminalHost) (*nativeClient, chan error, string) {
	t.Helper()
	serverEnd, clientEnd := net.Pipe()
	c := newNativeClient(t, clientEnd)
	t.Cleanup(func() { _ = c.conn.Close() })
	served := make(chan error, 1)
	quiet := slog.New(slog.DiscardHandler)
	go func() {
		served <- ServeWith(t.Context(), serverEnd, r.host, ServeOptions{Terminals: host, Logger: quiet})
	}()
	c.negotiate()
	return c, served, c.startThread(r)
}

func answerTerminalApprovalOnce(t *testing.T, c *nativeClient) {
	t.Helper()
	go func() {
		select {
		case request := <-c.requests:
			c.send(message{ID: request.ID, Result: jsontext.Value(`{"decision":"allowOnce"}`)})
		case <-time.After(waitLimit):
		}
	}()
}

func openWorkspaceTerminalLease(t *testing.T, c *nativeClient, threadID string) {
	t.Helper()
	var opened workspaceOpened
	c.ok("dalgona/workspace/open", map[string]any{"threadId": threadID, "root": "cwd", "rights": []string{"terminal"}, "operationId": rpc.NewOperationID()}, &opened)
	if opened.WorkspaceID == "" {
		t.Fatal("workspace open returned an empty workspace id")
	}
	var attached terminalShutdownAttached
	c.ok("dalgona/terminal/open", map[string]any{"workspaceId": opened.WorkspaceID, "columns": 80, "rows": 24, "operationId": rpc.NewOperationID()}, &attached)
	if attached.Attachment.WriterLease == "" {
		t.Fatal("terminal open granted no writer lease")
	}
}

func requireSingleTerminalAttached(t *testing.T, host *TerminalHost) {
	t.Helper()
	if got := len(host.all()); got != 1 {
		t.Fatalf("terminal host holds %d terminals, want 1", got)
	}
}

func closeTerminalTestConn(t *testing.T, c *nativeClient) {
	t.Helper()
	if closeErr := c.conn.Close(); closeErr != nil {
		t.Fatal(closeErr)
	}
}

func awaitTerminalLeaseShutdown(t *testing.T, served <-chan error) {
	t.Helper()
	select {
	case serveErr := <-served:
		if serveErr != nil {
			t.Fatalf("ServeWith returned %v", serveErr)
		}
	case <-time.After(time.Second):
		t.Fatal("shutdown waited for the terminal reconnect lease instead of detaching it")
	}
}

func requireTerminalWorkspaceHandlesClosed(t *testing.T, counter *shutdownHandleCounter) {
	t.Helper()
	counter.mu.Lock()
	defer counter.mu.Unlock()
	if counter.opened != 1 {
		t.Fatalf("opened %d workspace handles, want 1", counter.opened)
	}
	if counter.closed != counter.opened {
		t.Fatalf("shutdown left %d of %d workspace handles open", counter.opened-counter.closed, counter.opened)
	}
}

func releaseAllTerminalShutdownTerms(host *TerminalHost) {
	for _, term := range host.all() {
		term.release()
	}
}

func TestWorkspaceShutdownIgnoresTerminalLease(t *testing.T) {
	r := newRig(t, "")
	counter := &shutdownHandleCounter{}
	installTerminalShutdownRunner(t, r, counter)
	r.openHost()
	host := NewTerminalHost(transcriptEngine)
	c, served, threadID := dialTerminalShutdownServer(t, r, host)
	answerTerminalApprovalOnce(t, c)
	openWorkspaceTerminalLease(t, c, threadID)
	requireSingleTerminalAttached(t, host)
	closeTerminalTestConn(t, c)
	awaitTerminalLeaseShutdown(t, served)
	requireTerminalWorkspaceHandlesClosed(t, counter)
	releaseAllTerminalShutdownTerms(host)
}
