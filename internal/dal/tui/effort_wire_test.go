package tui

import (
	"bufio"
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const wireConfiguredEffort = "low"

type effortWire struct {
	t      *testing.T
	client *rpc.Conn
	notes  chan notificationMsg
	cwd    string
	peer   sync.WaitGroup

	side    net.Conn
	mu      sync.Mutex
	created int
	turns   int
	items   int
	threads map[string]*wireThread
}

type wireThread struct {
	id     string
	turn   string
	effort string
	live   bool
}

type effortHandler struct {
	done  <-chan struct{}
	notes chan notificationMsg
}

func (h *effortHandler) Notification(method string, params jsontext.Value) {
	select {
	case h.notes <- notificationMsg{method: method, params: append(jsontext.Value(nil), params...)}:
	case <-h.done:
	}
}

func (*effortHandler) Request(context.Context, string, jsontext.Value) (any, error) {
	return nil, errors.New("unexpected server request in effort lifecycle")
}

func startEffortWire(ctx context.Context, t *testing.T) *effortWire {
	t.Helper()
	handler := &effortHandler{done: ctx.Done(), notes: make(chan notificationMsg, 128)}
	left, right := net.Pipe()
	wire := &effortWire{
		t:       t,
		notes:   handler.notes,
		cwd:     t.TempDir(),
		side:    left,
		threads: map[string]*wireThread{},
	}
	client := rpc.Dial(ctx, right, handler)
	wire.client = client
	wire.peer.Go(wire.serve)
	t.Cleanup(func() {
		if err := client.Close(); err != nil {
			t.Errorf("close wire client: %v", err)
		}
		if err := left.Close(); err != nil {
			t.Errorf("close wire peer: %v", err)
		}
		wire.peer.Wait()
	})
	return wire
}

func (w *effortWire) serve() {
	scanner := bufio.NewScanner(w.side)
	scanner.Buffer(make([]byte, 4096), 1<<20)
	for scanner.Scan() {
		w.dispatch(scanner.Bytes())
	}
	if err := w.side.Close(); err != nil && !errors.Is(err, net.ErrClosed) {
		w.t.Errorf("close wire peer side: %v", err)
	}
}

type wireEnvelope struct {
	ID     jsontext.Value `json:"id"`
	Method string         `json:"method"`
	Params jsontext.Value `json:"params"`
}

func (w *effortWire) dispatch(line []byte) {
	var envelope wireEnvelope
	if err := json.Unmarshal(line, &envelope); err != nil {
		w.t.Errorf("wire peer decode request: %v", err)
		return
	}
	switch envelope.Method {
	case "thread/start":
		w.threadStart(envelope)
	case "thread/resume":
		w.threadResume(envelope)
	case "turn/start":
		w.turnStart(envelope)
	case "turn/interrupt":
		w.turnInterrupt(envelope)
	default:
		w.replyError(envelope.ID, "unsupported method "+envelope.Method)
	}
}

func (w *effortWire) threadStart(envelope wireEnvelope) {
	var request rpc.ThreadStartParams
	if err := json.Unmarshal(envelope.Params, &request); err != nil {
		w.replyError(envelope.ID, err.Error())
		return
	}
	thread := w.openWireThread()
	started := rpc.Thread{
		ID:     thread.id,
		Status: rpc.ThreadStatus{Type: "idle"},
	}
	w.reply(envelope.ID, rpc.ThreadStartResponse{
		ApprovalPolicy:  "never",
		Cwd:             request.Cwd,
		Model:           "probe/probe",
		Sandbox:         rpc.SandboxPolicy{Type: "off"},
		ReasoningEffort: wireConfiguredEffort,
		Thread:          started,
	})
	w.notify("thread/started", rpc.ThreadStartedNotification{Thread: started})
}

func (w *effortWire) openWireThread() *wireThread {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.created++
	thread := &wireThread{id: fmt.Sprintf("wire-%d", w.created)}
	w.threads[thread.id] = thread
	return thread
}

func (w *effortWire) threadResume(envelope wireEnvelope) {
	var request rpc.ThreadResumeParams
	if err := json.Unmarshal(envelope.Params, &request); err != nil {
		w.replyError(envelope.ID, err.Error())
		return
	}
	response, ok := w.threadSnapshot(request.ThreadID)
	if !ok {
		w.replyError(envelope.ID, "unknown thread "+request.ThreadID)
		return
	}
	w.reply(envelope.ID, response)
}

func (w *effortWire) threadSnapshot(threadID string) (rpc.ThreadStartResponse, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	thread, ok := w.threads[threadID]
	if !ok {
		return rpc.ThreadStartResponse{}, false
	}
	response := rpc.ThreadStartResponse{
		ApprovalPolicy:  "never",
		Cwd:             w.cwd,
		Model:           "probe/probe",
		Sandbox:         rpc.SandboxPolicy{Type: "off"},
		ReasoningEffort: wireConfiguredEffort,
		Thread: rpc.Thread{
			ID:     thread.id,
			Status: rpc.ThreadStatus{Type: "idle"},
		},
	}
	if thread.live {
		effort := thread.effort
		response.TurnEffort = &effort
		response.Thread.Status = rpc.ThreadStatus{Type: "active"}
		response.Thread.Turns = []rpc.Turn{{ID: thread.turn, Status: "inProgress"}}
	}
	return response, true
}

func (w *effortWire) turnStart(envelope wireEnvelope) {
	var request rpc.TurnStartParams
	if err := json.Unmarshal(envelope.Params, &request); err != nil {
		w.replyError(envelope.ID, err.Error())
		return
	}
	turn, effort, ok := w.beginTurn(request)
	if !ok {
		w.replyError(envelope.ID, "unknown thread "+request.ThreadID)
		return
	}
	running := rpc.Turn{ID: turn, Status: "inProgress"}
	w.reply(envelope.ID, rpc.TurnStartResponse{Turn: running})
	w.notify("turn/started", rpc.TurnStartedNotification{ThreadID: request.ThreadID, Turn: running})
	w.notify("dalgona/turn/effort", rpc.TurnEffortNotification{
		ThreadID: request.ThreadID, TurnID: turn, Effort: effort,
	})
}

func (w *effortWire) beginTurn(request rpc.TurnStartParams) (string, string, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	thread, ok := w.threads[request.ThreadID]
	if !ok {
		return "", "", false
	}
	w.turns++
	turn := fmt.Sprintf("turn-%d", w.turns)
	effort := w.resolveEffort(request)
	thread.turn, thread.effort, thread.live = turn, effort, true
	return turn, effort, true
}

func (w *effortWire) resolveEffort(request rpc.TurnStartParams) string {
	if request.Effort != "" {
		return request.Effort
	}
	if strings.Contains(wireInputText(request.Input), "ultrathink") {
		return "max"
	}
	return wireConfiguredEffort
}

func wireInputText(items []rpc.InputItem) string {
	var prompt strings.Builder
	for _, item := range items {
		prompt.WriteString(item.Text)
	}
	return prompt.String()
}

func (w *effortWire) turnInterrupt(envelope wireEnvelope) {
	var request rpc.TurnInterruptParams
	if err := json.Unmarshal(envelope.Params, &request); err != nil {
		w.replyError(envelope.ID, err.Error())
		return
	}
	turn, ok := w.endTurn(request.ThreadID)
	if !ok {
		w.replyError(envelope.ID, "unknown thread "+request.ThreadID)
		return
	}
	w.reply(envelope.ID, jsontext.Value("{}"))
	w.notify("turn/completed", rpc.TurnCompletedNotification{
		ThreadID: request.ThreadID,
		Turn:     rpc.Turn{ID: turn, Status: "interrupted"},
	})
}

func (w *effortWire) endTurn(threadID string) (string, bool) {
	w.mu.Lock()
	defer w.mu.Unlock()
	thread, ok := w.threads[threadID]
	if !ok {
		return "", false
	}
	turn := thread.turn
	thread.turn, thread.effort, thread.live = "", "", false
	return turn, true
}

func (w *effortWire) completeTurn(threadID, turnID string) {
	w.endTurn(threadID)
	w.notify("turn/completed", rpc.TurnCompletedNotification{
		ThreadID: threadID,
		Turn:     rpc.Turn{ID: turnID, Status: "completed"},
	})
}

func (w *effortWire) emitDelta(threadID string) {
	turn, item := w.nextDelta(threadID)
	w.notify("item/agentMessage/delta", rpc.AgentMessageDeltaNotification{
		ThreadID: threadID, TurnID: turn, ItemID: item, Delta: "partial",
	})
}

func (w *effortWire) nextDelta(threadID string) (string, string) {
	w.mu.Lock()
	defer w.mu.Unlock()
	w.items++
	item := fmt.Sprintf("item-%d", w.items)
	turn := ""
	if thread := w.threads[threadID]; thread != nil {
		turn = thread.turn
	}
	return turn, item
}

func (w *effortWire) call(ctx context.Context, t *testing.T, method string, params, result any) {
	t.Helper()
	if err := w.client.Call(ctx, method, params, result); err != nil {
		t.Fatalf("%s: %v", method, err)
	}
}

func (w *effortWire) openThread(ctx context.Context, t *testing.T) model {
	t.Helper()
	var started rpc.ThreadStartResponse
	w.call(ctx, t, "thread/start", rpc.ThreadStartParams{Cwd: w.cwd}, &started)
	m := bandModel(w.cwd)
	m.queue = nil
	m.conn.client = w.client
	m.applyNewThread(&started)
	m.thread.levels = map[string][]string{"probe/probe": {"low", "medium", "high", "max"}}
	w.applyUntil(ctx, t, &m, "thread/started")
	return m
}

func (w *effortWire) spawnThread(ctx context.Context, t *testing.T, m *model) rpc.Thread {
	t.Helper()
	var started rpc.ThreadStartResponse
	w.call(ctx, t, "thread/start", rpc.ThreadStartParams{Cwd: w.cwd}, &started)
	w.applyUntil(ctx, t, m, "thread/started")
	return started.Thread
}

func (w *effortWire) interruptTurn(ctx context.Context, t *testing.T, threadID, turnID string) {
	t.Helper()
	w.call(ctx, t, "turn/interrupt", rpc.TurnInterruptParams{ThreadID: threadID, TurnID: turnID}, nil)
}

func (w *effortWire) applyUntil(ctx context.Context, t *testing.T, m *model, method string) {
	t.Helper()
	for {
		select {
		case note := <-w.notes:
			if _, err := m.applyDecodedNotification(note); err != nil {
				t.Fatalf("apply %s: %v", note.method, err)
			}
			if note.method == method {
				return
			}
		case <-ctx.Done():
			t.Fatalf("waiting for %s: %v", method, context.Cause(ctx))
		}
	}
}

func (w *effortWire) consumeUntil(ctx context.Context, t *testing.T, m *model, method string) {
	t.Helper()
	for {
		select {
		case note := <-w.notes:
			hit := note.method == method
			m.applyNotification(note)
			if hit {
				return
			}
		case <-ctx.Done():
			t.Fatalf("waiting for %s: %v", method, context.Cause(ctx))
		}
	}
}

type wireOut struct {
	JSONRPC string         `json:"jsonrpc"`
	ID      jsontext.Value `json:"id,omitzero"`
	Method  string         `json:"method,omitzero"`
	Params  jsontext.Value `json:"params,omitzero"`
	Result  jsontext.Value `json:"result,omitzero"`
	Error   *wireFault     `json:"error,omitzero"`
}

type wireFault struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

func (w *effortWire) reply(id jsontext.Value, result any) {
	w.write(wireOut{ID: id, Result: w.encode(result)})
}

func (w *effortWire) replyError(id jsontext.Value, message string) {
	w.write(wireOut{ID: id, Error: &wireFault{Code: -32601, Message: message}})
}

func (w *effortWire) notify(method string, params any) {
	w.write(wireOut{Method: method, Params: w.encode(params)})
}

func (w *effortWire) encode(value any) jsontext.Value {
	raw, err := json.Marshal(value)
	if err != nil {
		w.t.Errorf("wire peer encode: %v", err)
		return jsontext.Value("null")
	}
	return raw
}

func (w *effortWire) write(message wireOut) {
	message.JSONRPC = "2.0"
	raw, err := json.Marshal(message)
	if err != nil {
		w.t.Errorf("wire peer frame: %v", err)
		return
	}
	if _, err := fmt.Fprintf(w.side, "%s\n", raw); err != nil && !errors.Is(err, io.ErrClosedPipe) && !errors.Is(err, net.ErrClosed) {
		w.t.Errorf("wire peer write: %v", err)
	}
}
