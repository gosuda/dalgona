package cursor_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"io"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/cursor"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

const waitLimit = 5 * time.Second

type fakeConn struct {
	t       *testing.T
	writer  http.ResponseWriter
	flusher http.Flusher
	inbound chan *wire.AgentClientMessage
	run     *wire.AgentRunRequest
}

func (c *fakeConn) send(message *wire.AgentServerMessage) {
	c.t.Helper()
	frame := wire.AppendFrame(nil, 0, wire.AppendAgentServerMessage(nil, message))
	if _, err := c.writer.Write(frame); err != nil {
		c.t.Errorf("write server frame: %v", err)
	}
	c.flusher.Flush()
}

func (c *fakeConn) sendExec(exec *wire.ExecServerMessage) {
	c.send(&wire.AgentServerMessage{Exec: exec})
}

func (c *fakeConn) endTurn() {
	c.send(&wire.AgentServerMessage{Interaction: &wire.InteractionUpdate{Ended: &wire.TurnEndedUpdate{Input: 100, Output: 7, CacheRead: 20, CacheWrite: 10}}})
}

func abortf(t *testing.T, format string, args ...any) {
	t.Helper()
	t.Errorf(format, args...)
	panic(http.ErrAbortHandler)
}

func (c *fakeConn) next() *wire.AgentClientMessage {
	c.t.Helper()
	timer := time.NewTimer(waitLimit)
	defer timer.Stop()
	for {
		select {
		case message, ok := <-c.inbound:
			if !ok {
				abortf(c.t, "client stream closed")
			}
			if !isHeartbeat(message) {
				return message
			}
		case <-timer.C:
			abortf(c.t, "no client message")
		}
	}
}

func isHeartbeat(message *wire.AgentClientMessage) bool {
	if message.MessageNumber == wire.ClientHeartbeatNumber {
		return true
	}
	if message.MessageNumber != wire.ClientExecControlNumber {
		return false
	}
	var control wire.ExecClientControl
	return wire.ConsumeExecClientControl(message.MessageView, &control) == nil && control.Kind == wire.ExecHeartbeat
}

func (c *fakeConn) execReply() *wire.ExecClientMessage {
	c.t.Helper()
	message := c.next()
	if message.Exec == nil {
		abortf(c.t, "client message = %#v, want exec reply", message)
	}
	return message.Exec
}

func (c *fakeConn) expectStreamClose(id uint32) {
	c.t.Helper()
	message := c.next()
	var control wire.ExecClientControl
	if message.MessageNumber != wire.ClientExecControlNumber || wire.ConsumeExecClientControl(message.MessageView, &control) != nil ||
		control.Kind != wire.ExecStreamClose || control.ID != id {
		abortf(c.t, "message = %#v, want stream close for %d", message, id)
	}
}

type fakeCursor struct {
	t      *testing.T
	runs   atomic.Int32
	mu     sync.Mutex
	script []func(*fakeConn)
}

func (f *fakeCursor) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.ProtoMajor != 2 || r.Header.Get("Authorization") != "Bearer cursor-token" || r.Header.Get("X-Cursor-Client-Type") != "cli" {
		f.t.Errorf("proto = %d, headers = %v", r.ProtoMajor, r.Header)
	}
	if r.URL.Path == "/agent.v1.AgentService/GetUsableModels" {
		serveModels(w)
		return
	}
	if r.Header.Get("Content-Type") != "application/connect+proto" {
		f.t.Errorf("run content type = %q", r.Header.Get("Content-Type"))
	}
	f.runs.Add(1)
	conn := &fakeConn{t: f.t, writer: w, flusher: w.(http.Flusher), inbound: make(chan *wire.AgentClientMessage, 64)}
	go readClient(r.Body, conn.inbound)
	w.Header().Set("Content-Type", "application/connect+proto")
	w.WriteHeader(http.StatusOK)
	conn.flusher.Flush()
	first := conn.next()
	if first.Run == nil {
		abortf(f.t, "first client message = %#v, want run request", first)
	}
	conn.run = first.Run
	f.nextScript()(conn)
}

func (f *fakeCursor) nextScript() func(*fakeConn) {
	f.mu.Lock()
	defer f.mu.Unlock()
	script := f.script[0]
	if len(f.script) > 1 {
		f.script = f.script[1:]
	}
	return script
}

func readClient(body io.Reader, inbound chan<- *wire.AgentClientMessage) {
	defer close(inbound)
	for {
		frame, err := wire.ReadFrame(body, nil, 1<<24)
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

func serveModels(w http.ResponseWriter) {
	response := wire.GetUsableModelsResponse{Models: []wire.ModelDetails{{ModelIDView: []byte("gpt-5.5")}, {ModelIDView: []byte(" composer-2.5 ")}, {ModelIDView: []byte("gpt-5.5")}}}
	_, _ = w.Write(wire.AppendFrame(nil, 0, wire.AppendGetUsableModelsResponse(nil, &response)))
}

func newServer(t *testing.T, scripts ...func(*fakeConn)) (*fakeCursor, *httptest.Server) {
	t.Helper()
	fake := &fakeCursor{t: t, script: scripts}
	server := httptest.NewUnstartedServer(fake)
	server.EnableHTTP2 = true
	server.StartTLS()
	t.Cleanup(server.Close)
	return fake, server
}

func newProvider(server *httptest.Server, idle time.Duration) *cursor.Provider {
	return cursor.New(cursor.Config{
		BaseURL: server.URL,
		Token:   func(context.Context) (string, error) { return "cursor-token", nil },
		Client:  server.Client(),
		Retry: model.RetryOptions{
			StartTimeout: 2 * time.Second,
			IdleTimeout:  idle,
			Sleep:        func(context.Context, time.Duration) error { return nil },
		},
	})
}

type result struct {
	text, reasoning string
	calls           []model.ToolCall
	usage           model.Usage
	stop            string
	failure         *model.Error
}

func drain(t *testing.T, stream model.Stream) result {
	t.Helper()
	var out result
	for event := range stream.Events() {
		switch value := event.(type) {
		case model.TextDelta:
			out.text += value.Text
		case model.ReasoningDelta:
			out.reasoning += value.Text
		case model.ToolCall:
			out.calls = append(out.calls, value)
		case model.Usage:
			out.usage = value
		case model.Done:
			out.stop = value.StopReason
		case model.Error:
			out.failure = &value
		}
	}
	return out
}

func userRequest(text string) model.Request {
	return model.Request{Model: "gpt-5.5", System: "be terse", Messages: []model.Message{{Role: "user", Text: text}}}
}

type inBandCounter struct {
	calls atomic.Int32
	mu    sync.Mutex
	seen  []model.ToolCall
	delay time.Duration
}

func (c *inBandCounter) run(_ context.Context, call model.ToolCall) (model.ToolResult, error) {
	c.calls.Add(1)
	c.mu.Lock()
	c.seen = append(c.seen, call)
	c.mu.Unlock()
	time.Sleep(c.delay)
	return model.ToolResult{ID: call.ID, Name: call.Name, Output: []byte(`"hi\n"`)}, nil
}

func shellExec(id uint32) *wire.ExecServerMessage {
	return &wire.ExecServerMessage{ID: id, ExecIDView: []byte("exec-1"), Shell: &wire.ShellArgs{CommandView: []byte("echo hi"), ToolCallIDView: []byte("call-1")}}
}

func TestTextTurnAnswersBlobAndContextRequests(t *testing.T) {
	_, server := newServer(t, func(c *fakeConn) {
		roots := c.run.Conversation.RootPromptMessages
		c.send(&wire.AgentServerMessage{MessageNumber: wire.ServerKvNumber, MessageView: wire.AppendKvServerMessage(nil, &wire.KvServerMessage{ID: 4, Get: &wire.GetBlobArgs{BlobIDView: roots[0]}})})
		checkSystemBlob(t, c.next())
		c.sendExec(&wire.ExecServerMessage{ID: 2, RequestContext: &wire.RequestContextArgs{}})
		checkContextTools(t, c.execReply())
		c.expectStreamClose(2)
		c.send(&wire.AgentServerMessage{Interaction: &wire.InteractionUpdate{Thinking: &wire.ThinkingDelta{TextView: []byte("plan")}}})
		c.send(&wire.AgentServerMessage{Interaction: &wire.InteractionUpdate{Text: &wire.TextDelta{TextView: []byte("hello")}}})
		c.endTurn()
	})
	request := userRequest("hi")
	request.Tools = []model.Tool{
		{Name: "read", Parameters: []byte(`{"type":"object","properties":{"path":{"anyOf":[{"type":"string"}],"type":"string"}}}`)},
		{Name: "exec", Parameters: []byte(`{"type":"object"}`)},
	}
	stream, err := newProvider(server, time.Second).Stream(t.Context(), request)
	if err != nil {
		t.Fatal(err)
	}
	out := drain(t, stream)
	if out.failure != nil || out.text != "hello" || out.reasoning != "plan" || out.stop != "stop" {
		t.Fatalf("result = %+v", out)
	}
	if out.usage != (model.Usage{Input: 70, Output: 7, CacheRead: 20, CacheWrite: 10}) {
		t.Fatalf("usage = %+v", out.usage)
	}
}

func checkSystemBlob(t *testing.T, message *wire.AgentClientMessage) {
	t.Helper()
	var reply wire.KvClientMessage
	if message.MessageNumber != wire.ClientKvNumber || wire.ConsumeKvClientMessage(message.MessageView, &reply) != nil || reply.ID != 4 || reply.Get == nil {
		abortf(t, "kv reply = %#v", message)
	}
	if string(reply.Get.BlobDataView) != `{"role":"system","content":"be terse"}` {
		abortf(t, "system blob = %s", reply.Get.BlobDataView)
	}
}

func checkContextTools(t *testing.T, reply *wire.ExecClientMessage) {
	t.Helper()
	if reply.ID != 2 || reply.RequestContextResult == nil || reply.RequestContextResult.Success == nil {
		abortf(t, "context reply = %#v", reply)
	}
	tools := reply.RequestContextResult.Success.RequestContext.Tools
	if len(tools) != 1 || string(tools[0].NameView) != "read" {
		abortf(t, "tools = %#v", tools)
	}
	schema, err := wire.ValueJSON(tools[0].InputSchemaView)
	if err != nil || string(schema) != `{"type":"object","properties":{"path":{"type":"string"}}}` {
		abortf(t, "schema = %s, err = %v", schema, err)
	}
}

func TestInBandExecRunsOnceAndEmitsResolvedCall(t *testing.T) {
	counter := &inBandCounter{}
	_, server := newServer(t, func(c *fakeConn) {
		c.sendExec(shellExec(9))
		checkShellReply(t, c.execReply())
		c.expectStreamClose(9)
		c.endTurn()
	})
	stream, err := newProvider(server, time.Second).StreamWith(t.Context(), userRequest("run it"), cursor.StreamOptions{InBand: counter.run})
	if err != nil {
		t.Fatal(err)
	}
	out := drain(t, stream)
	if out.failure != nil || out.stop != "stop" || counter.calls.Load() != 1 {
		t.Fatalf("result = %+v, in-band calls = %d", out, counter.calls.Load())
	}
	checkResolvedExec(t, out.calls)
	requireStableInBandCalls(t, counter, 1)
}

func requireStableInBandCalls(t *testing.T, counter *inBandCounter, want int32) {
	t.Helper()
	quiescent := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(quiescent) {
		if got := counter.calls.Load(); got != want {
			t.Fatalf("in-band calls = %d, want %d: duplicate execution after stream end", got, want)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func checkResolvedExec(t *testing.T, calls []model.ToolCall) {
	t.Helper()
	if len(calls) != 1 || !calls[0].Resolved || calls[0].Name != "exec" || calls[0].ID != "call-1" || string(calls[0].Args) != `{"command":"echo hi"}` {
		t.Fatalf("calls = %+v", calls)
	}
}

func checkShellReply(t *testing.T, reply *wire.ExecClientMessage) {
	t.Helper()
	var shell wire.ShellResult
	if reply.ID != 9 || string(reply.ExecIDView) != "exec-1" || reply.ResultNumber != wire.ExecShellResultNumber || wire.ConsumeShellResult(reply.ResultView, &shell) != nil {
		abortf(t, "shell reply = %#v", reply)
	}
	if shell.Success == nil || string(shell.Success.StdoutView) != "hi\n" || string(shell.Success.CommandView) != "echo hi" {
		abortf(t, "shell result = %#v", shell)
	}
}

func TestDropAfterInBandFailsWithoutSecondRun(t *testing.T) {
	counter := &inBandCounter{}
	fake, server := newServer(t, func(c *fakeConn) {
		c.sendExec(shellExec(9))
		c.execReply()
	})
	stream, err := newProvider(server, time.Second).StreamWith(t.Context(), userRequest("run it"), cursor.StreamOptions{InBand: counter.run})
	if err != nil {
		t.Fatal(err)
	}
	out := drain(t, stream)
	if out.failure == nil || counter.calls.Load() != 1 || fake.runs.Load() != 1 {
		t.Fatalf("failure = %+v, in-band calls = %d, runs = %d", out.failure, counter.calls.Load(), fake.runs.Load())
	}
}

type heldBody struct {
	io.ReadCloser
	armed  *atomic.Bool
	held   bool
	closed chan struct{}
	once   sync.Once
}

func (b *heldBody) Read(p []byte) (int, error) {
	if b.held {
		<-b.closed
		return b.ReadCloser.Read(p)
	}
	n, err := b.ReadCloser.Read(p)
	b.held = n > 0 && b.armed.Load()
	return n, err
}

func (b *heldBody) Close() error {
	err := b.ReadCloser.Close()
	b.once.Do(func() { close(b.closed) })
	return err
}

type holdAfterReply struct {
	base  http.RoundTripper
	armed *atomic.Bool
}

func (h holdAfterReply) RoundTrip(req *http.Request) (*http.Response, error) {
	if req.Body != nil {
		req = req.Clone(req.Context())
		req.Body = &heldBody{ReadCloser: req.Body, armed: h.armed, closed: make(chan struct{})}
	}
	return h.base.RoundTrip(req)
}

func TestStreamCloseAfterServerEndKeepsTurn(t *testing.T) {
	counter := &inBandCounter{}
	_, server := newServer(t, func(c *fakeConn) {
		c.sendExec(shellExec(9))
		checkShellReply(t, c.execReply())
		c.endTurn()
	})
	armed := &atomic.Bool{}
	client := *server.Client()
	client.Transport = holdAfterReply{base: client.Transport, armed: armed}
	provider := cursor.New(cursor.Config{
		BaseURL: server.URL, Client: &client,
		Token: func(context.Context) (string, error) { return "cursor-token", nil },
		Retry: model.RetryOptions{StartTimeout: 2 * time.Second, IdleTimeout: time.Second, Sleep: func(context.Context, time.Duration) error { return nil }},
	})
	inBand := func(ctx context.Context, call model.ToolCall) (model.ToolResult, error) {
		armed.Store(true)
		return counter.run(ctx, call)
	}
	stream, err := provider.StreamWith(t.Context(), userRequest("run it"), cursor.StreamOptions{InBand: inBand})
	if err != nil {
		t.Fatal(err)
	}
	out := drain(t, stream)
	if out.failure != nil || out.stop != "stop" || counter.calls.Load() != 1 {
		t.Fatalf("result = %+v, failure = %+v, in-band calls = %d", out, out.failure, counter.calls.Load())
	}
	checkResolvedExec(t, out.calls)
}

func TestDryRunMcpFrameRunsNothing(t *testing.T) {
	counter := &inBandCounter{}
	_, server := newServer(t, func(c *fakeConn) {
		c.sendExec(&wire.ExecServerMessage{ID: 3, Mcp: &wire.McpArgs{NameView: []byte("write"), ToolNameView: []byte("write"), ToolCallIDView: []byte("m1"), ApprovalOnly: true}})
		reply := c.execReply()
		if reply.McpResult == nil || reply.McpResult.ResultNumber != wire.McpResultRejectedNumber {
			t.Errorf("dry-run reply = %#v", reply)
		}
		c.expectStreamClose(3)
		c.endTurn()
	})
	stream, err := newProvider(server, time.Second).StreamWith(t.Context(), userRequest("go"), cursor.StreamOptions{InBand: counter.run})
	if err != nil {
		t.Fatal(err)
	}
	out := drain(t, stream)
	if out.failure != nil || counter.calls.Load() != 0 || len(out.calls) != 0 {
		t.Fatalf("result = %+v, in-band calls = %d", out, counter.calls.Load())
	}
}

func TestMcpCallDecodesArgumentsAndReturnsText(t *testing.T) {
	counter := &inBandCounter{}
	path, err := wire.AppendValueFromJSON(nil, []byte(`"a.go"`))
	if err != nil {
		t.Fatal(err)
	}
	_, server := newServer(t, func(c *fakeConn) {
		c.sendExec(&wire.ExecServerMessage{ID: 5, Mcp: &wire.McpArgs{ToolNameView: []byte("read"), ToolCallIDView: []byte("m2"), Args: []wire.McpArgument{{NameView: []byte("path"), ValueView: path}}}})
		checkMcpReply(t, c.execReply())
		c.expectStreamClose(5)
		c.endTurn()
	})
	stream, err := newProvider(server, time.Second).StreamWith(t.Context(), userRequest("go"), cursor.StreamOptions{InBand: counter.run})
	if err != nil {
		t.Fatal(err)
	}
	out := drain(t, stream)
	if len(counter.seen) != 1 || counter.seen[0].Name != "read" || string(counter.seen[0].Args) != `{"path":"a.go"}` || len(out.calls) != 1 {
		t.Fatalf("in-band calls = %+v, events = %+v", counter.seen, out)
	}
}

func checkMcpReply(t *testing.T, reply *wire.ExecClientMessage) {
	t.Helper()
	success := reply.McpResult.Success
	if success == nil || len(success.Content) != 1 || success.IsError {
		abortf(t, "mcp reply = %#v", reply.McpResult)
	}
	text, err := wire.ConsumeTextMessage(success.Content[0].ContentView)
	if err != nil || string(text) != "hi\n" {
		abortf(t, "mcp text = %q, err = %v", text, err)
	}
}

func TestIdleTimerPausesDuringSlowInBand(t *testing.T) {
	counter := &inBandCounter{delay: 400 * time.Millisecond}
	_, server := newServer(t, func(c *fakeConn) {
		c.sendExec(shellExec(9))
		c.execReply()
		c.expectStreamClose(9)
		c.endTurn()
	})
	stream, err := newProvider(server, 100*time.Millisecond).StreamWith(t.Context(), userRequest("slow"), cursor.StreamOptions{InBand: counter.run})
	if err != nil {
		t.Fatal(err)
	}
	if out := drain(t, stream); out.failure != nil || out.stop != "stop" {
		t.Fatalf("result = %+v", out)
	}
}

func TestMissingInBandReportsUnavailable(t *testing.T) {
	_, server := newServer(t, func(c *fakeConn) {
		c.sendExec(shellExec(9))
		var shell wire.ShellResult
		reply := c.execReply()
		if wire.ConsumeShellResult(reply.ResultView, &shell) != nil || shell.Failure == nil || !strings.Contains(string(shell.Failure.StdoutView), "in-band tool execution is unavailable") {
			t.Errorf("shell reply = %#v", shell)
		}
		c.expectStreamClose(9)
		c.endTurn()
	})
	stream, err := newProvider(server, time.Second).Stream(t.Context(), userRequest("run"))
	if err != nil {
		t.Fatal(err)
	}
	if out := drain(t, stream); out.failure != nil || len(out.calls) != 0 {
		t.Fatalf("result = %+v", out)
	}
}

func TestStateDirPersistsConversation(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "thread-1")
	serverBlob := []byte("server-turn")
	blobID := sha256.Sum256(serverBlob)
	var firstID string
	_, server := newServer(t, func(c *fakeConn) {
		firstID = string(c.run.ConversationIDView)
		checkpoint := wire.AppendConversationState(nil, &wire.ConversationState{RootPromptMessages: c.run.Conversation.RootPromptMessages[:1], SummaryView: []byte("summary")})
		c.send(&wire.AgentServerMessage{MessageNumber: wire.ServerCheckpointNumber, MessageView: checkpoint})
		c.send(&wire.AgentServerMessage{MessageNumber: wire.ServerKvNumber, MessageView: wire.AppendKvServerMessage(nil, &wire.KvServerMessage{ID: 1, Set: &wire.SetBlobArgs{BlobIDView: blobID[:], BlobDataView: serverBlob}})})
		c.next()
		c.endTurn()
	}, func(c *fakeConn) {
		if string(c.run.ConversationIDView) != firstID || string(c.run.Conversation.SummaryView) != "summary" {
			t.Errorf("second run id = %s, summary = %q", c.run.ConversationIDView, c.run.Conversation.SummaryView)
		}
		c.send(&wire.AgentServerMessage{MessageNumber: wire.ServerKvNumber, MessageView: wire.AppendKvServerMessage(nil, &wire.KvServerMessage{ID: 2, Get: &wire.GetBlobArgs{BlobIDView: blobID[:]}})})
		var reply wire.KvClientMessage
		if wire.ConsumeKvClientMessage(c.next().MessageView, &reply) != nil || reply.Get == nil || !bytes.Equal(reply.Get.BlobDataView, serverBlob) {
			t.Errorf("blob reply = %#v", reply)
		}
		c.endTurn()
	})
	for _, text := range []string{"one", "two"} {
		provider := newProvider(server, time.Second)
		stream, err := provider.StreamWith(t.Context(), userRequest(text), cursor.StreamOptions{StateDir: dir})
		if err != nil {
			t.Fatal(err)
		}
		if out := drain(t, stream); out.failure != nil {
			t.Fatalf("turn %s failed: %v", text, out.failure.Err)
		}
	}
	checkPrivateFile(t, filepath.Join(dir, "checkpoint"))
	checkPrivateFile(t, filepath.Join(dir, "blobs", hex.EncodeToString(blobID[:])))
}

func TestRetryableTrailerRetriesTurn(t *testing.T) {
	fake, server := newServer(t, func(c *fakeConn) {
		frame := wire.AppendFrame(nil, wire.EndStream, []byte(`{"error":{"code":"resource_exhausted","message":"busy"}}`))
		_, _ = c.writer.Write(frame)
		c.flusher.Flush()
	}, func(c *fakeConn) {
		c.send(&wire.AgentServerMessage{Interaction: &wire.InteractionUpdate{Text: &wire.TextDelta{TextView: []byte("ok")}}})
		c.endTurn()
	})
	stream, err := newProvider(server, time.Second).Stream(t.Context(), userRequest("hi"))
	if err != nil {
		t.Fatal(err)
	}
	if out := drain(t, stream); out.failure != nil || out.text != "ok" || fake.runs.Load() != 2 {
		t.Fatalf("result = %+v, runs = %d", out, fake.runs.Load())
	}
}

func TestModelsListsUsableModels(t *testing.T) {
	_, server := newServer(t, func(*fakeConn) {})
	models, err := newProvider(server, time.Second).Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if len(models) != 2 || models[0].ID != "gpt-5.5" || models[1].ID != "composer-2.5" || models[0].Context != 128000 {
		t.Fatalf("models = %+v", models)
	}
}
