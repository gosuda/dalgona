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

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/connectwire"
	"github.com/gosuda/dalgona/internal/dal/model/cursor"
	"github.com/gosuda/dalgona/internal/dal/model/cursor/wire"
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

func (c *cursorConn) invoke(id uint32, callID, name, arg, value string) string {
	encoded, err := wire.AppendValueFromJSON(nil, jsonString(value))
	if err != nil {
		c.t.Errorf("encode Cursor argument: %v", err)
		return ""
	}
	c.send(&wire.AgentServerMessage{Exec: &wire.ExecServerMessage{
		ID: id, Mcp: &wire.McpArgs{
			ToolNameView: []byte(name), ToolCallIDView: []byte(callID),
			Args: []wire.McpArgument{{NameView: []byte(arg), ValueView: encoded}},
		},
	}})
	reply := c.awaitExecReply()
	if reply == nil || reply.McpResult == nil || reply.McpResult.Success == nil || len(reply.McpResult.Success.Content) != 1 {
		c.t.Errorf("Cursor reply to %s = %+v", callID, reply)
		return ""
	}
	text, err := wire.ConsumeTextMessage(reply.McpResult.Success.Content[0].ContentView)
	if err != nil {
		c.t.Errorf("decode Cursor reply: %v", err)
	}
	return string(text)
}

func TestCursorRepeatedHashlineCallCompletesTurn(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light", withCursor(t, func(c *cursorConn) {
		first := c.invoke(1, "p1", "patch", "input", createBody)
		second := c.invoke(2, "p1", "patch", "input", createBody)
		if first == "" || second != first {
			t.Errorf("Cursor replies to original and replay = %q and %q", first, second)
		}
		c.endTurn("done")
	}))
	thread := h.idleThread("create a file")
	events := h.events(thread.ID())
	if end := lastTurn(events); end.Status != statusCompleted {
		t.Fatalf("turn did not complete: %+v", end)
	}
	if startsOf(events, "p1") != 1 || approvalsOf(events, "p1") != 1 || !completedOutputs(events)["p1"].OK {
		t.Fatalf("expected one successful execution: %v", h.types(thread.ID()))
	}
}

func TestCursorInBandEditUsesStreamingRequestCutoff(t *testing.T) {
	t.Parallel()
	h := newProfileHarness(t, "hashline-light", withCursor(t, func(c *cursorConn) {
		read := c.invoke(1, "r1", "read", "path", "a.txt")
		ref := headerReference([]byte(read), "a.txt", "")
		if ref == "" {
			t.Errorf("Cursor read reply has no reference: %q", read)
			c.endTurn("done")
			return
		}
		c.invoke(2, "p1", "patch", "input", "[a.txt@"+ref+"]\nPUT 1:\n+after\n")
		c.endTurn("done")
	}))
	h.write("a.txt", "before\n")
	thread := h.idleThread("read and edit a file")
	events := h.events(thread.ID())
	edit := completedOutputs(events)["p1"]
	requireHashlineRecovery(t, edit.Meta, hashlineWant{"hashline-light", "needs_context", "none", "reference_unknown", "proof"}, unknownRead)
	if edit.OK || startsOf(events, "p1") != 0 || h.read("a.txt") != "before\n" {
		t.Fatalf("the late reference authorized a write: %+v", edit)
	}
	_, readCarrier := carrierOf(t, events, "r1")
	_, editCarrier := carrierOf(t, events, "p1")
	if editCarrier.ContextSeq != readCarrier.ContextSeq || editCarrier.ContextSeq >= completionSeq(t, events, "r1") {
		t.Fatalf("in-band cutoffs read=%d, edit=%d; want the same cutoff before the read delivery", readCarrier.ContextSeq, editCarrier.ContextSeq)
	}
}

func TestRecoverCursorTurnReviewsNewInBandEdit(t *testing.T) {
	t.Parallel()
	judge := &judgeStub{}
	h := newProfileHarness(t, "hashline-light", withCursor(t, func(c *cursorConn) {
		c.invoke(1, "p1", "patch", "input", createBody)
		c.endTurn("done")
	}), func(h *harness) {
		h.cfg.EditJudge = judgeFor(judge)
		h.cfg.Config.EditReview.Enabled = true
	})
	h.seed(openTurn("create a file")...)
	events := h.recoverSession()
	judged := judgedOf(events)
	if end := lastTurn(events); end.Status != statusCompleted || len(judged) != 1 || judged[0].CallID != "p1" || len(editStatesOf(judge)) != 1 {
		t.Fatalf("turn ended %+v with %v reviews and %d judge requests", end, judged, len(editStatesOf(judge)))
	}
}
