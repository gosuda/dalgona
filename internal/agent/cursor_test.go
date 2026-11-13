package agent

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"github.com/gosuda/dalgona/internal/model/cursor"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

type cursorConn struct {
	t       *testing.T
	writer  http.ResponseWriter
	inbound chan *wire.AgentClientMessage
}

func (c *cursorConn) send(message *wire.AgentServerMessage) {
	if _, err := c.writer.Write(connectwire.AppendFrame(nil, 0, wire.AppendAgentServerMessage(nil, message))); err != nil {
		c.t.Errorf("write cursor frame: %v", err)
	}
	c.writer.(http.Flusher).Flush()
}

func (c *cursorConn) awaitExecReply() *wire.ExecClientMessage {
	timer := time.NewTimer(10 * time.Second)
	defer timer.Stop()
	for {
		select {
		case message, ok := <-c.inbound:
			if !ok {
				c.t.Error("cursor client closed before the exec reply")
				return nil
			}
			if message.Exec != nil {
				return message.Exec
			}
		case <-timer.C:
			c.t.Error("no exec reply")
			return nil
		}
	}
}

func (c *cursorConn) endTurn(text string) {
	c.send(&wire.AgentServerMessage{Interaction: &wire.InteractionUpdate{Text: &wire.TextDelta{TextView: []byte(text)}}})
	c.send(&wire.AgentServerMessage{Interaction: &wire.InteractionUpdate{Ended: &wire.TurnEndedUpdate{Input: 10, Output: 2}}})
}

type fakeCursorServer struct {
	t       *testing.T
	mu      sync.Mutex
	scripts []func(*cursorConn)
}

func (f *fakeCursorServer) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if strings.HasSuffix(r.URL.Path, "/GetUsableModels") {
		response := wire.GetUsableModelsResponse{Models: []wire.ModelDetails{{ModelIDView: []byte("gpt-5.5")}}}
		_, _ = w.Write(connectwire.AppendFrame(nil, 0, wire.AppendGetUsableModelsResponse(nil, &response)))
		return
	}
	conn := &cursorConn{t: f.t, writer: w, inbound: make(chan *wire.AgentClientMessage, 64)}
	go readCursorClient(r.Body, conn.inbound)
	w.Header().Set("Content-Type", "application/connect+proto")
	w.WriteHeader(http.StatusOK)
	w.(http.Flusher).Flush()
	f.mu.Lock()
	script := f.scripts[0]
	f.scripts = f.scripts[min(1, len(f.scripts)-1):]
	f.mu.Unlock()
	script(conn)
}

func readCursorClient(body io.Reader, inbound chan<- *wire.AgentClientMessage) {
	defer close(inbound)
	for {
		frame, err := connectwire.ReadFrame(body, nil, 1<<24)
		if err != nil {
			return
		}
		var message wire.AgentClientMessage
		if wire.ConsumeAgentClientMessage(frame.Payload, &message) != nil {
			return
		}
		inbound <- &message
	}
}

func withCursor(t *testing.T, scripts ...func(*cursorConn)) func(*harness) {
	fake := &fakeCursorServer{t: t, scripts: scripts}
	server := httptest.NewUnstartedServer(fake)
	server.EnableHTTP2 = true
	server.StartTLS()
	t.Cleanup(server.Close)
	provider := cursor.New(cursor.Config{
		BaseURL: server.URL, Client: server.Client(),
		Token: func(context.Context) (string, error) { return "cursor-token", nil },
		Retry: model.RetryOptions{StartTimeout: 5 * time.Second, IdleTimeout: 5 * time.Second, Sleep: func(context.Context, time.Duration) error { return nil }},
	})
	return func(h *harness) {
		h.cfg.Config.Model = "cursor/gpt-5.5"
		h.cfg.Config.Approval = config.ApprovalYolo
		h.cfg.Providers = func(RetryFunc) (*model.Registry, error) {
			registry := model.NewRegistry()
			return registry, registry.Register("cursor", provider)
		}
	}
}

func shellStdout(reply *wire.ExecClientMessage) string {
	var shell wire.ShellResult
	if reply == nil || wire.ConsumeShellResult(reply.ResultView, &shell) != nil || shell.Success == nil {
		return ""
	}
	return string(shell.Success.StdoutView)
}

func TestCursorInBandExecRunsThroughTheGate(t *testing.T) {
	t.Parallel()
	stdout := make(chan string, 1)
	h := newHarness(t, withCursor(t,
		func(c *cursorConn) {
			c.send(&wire.AgentServerMessage{Exec: &wire.ExecServerMessage{
				ID: 9, ExecIDView: []byte("exec-1"), Shell: &wire.ShellArgs{CommandView: []byte("echo in-band"), ToolCallIDView: []byte("call-1")},
			}})
			stdout <- shellStdout(c.awaitExecReply())
			c.endTurn("ran it")
		},
		func(c *cursorConn) { c.endTurn("done") },
	))
	thread := h.idleThread("run it in band")
	events := h.events(thread.ID())
	if got := <-stdout; !strings.Contains(got, "in-band") {
		t.Fatalf("shell stdout sent to cursor = %q", got)
	}
	completed := completedOutputs(events)["call-1"]
	if !completed.OK || !strings.Contains(completed.Output, "in-band") || resolutions(events)["call-1"].By != "mode:yolo" {
		t.Fatalf("in-band completion = %+v", completed)
	}
	if countType(events, event.TypeToolStarted) != 1 || countType(events, event.TypeAssistantMessage) != 2 {
		t.Fatalf("events = %v", h.types(thread.ID()))
	}
}
