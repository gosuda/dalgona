package agent

import (
	"bytes"
	"context"
	"encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/openai"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/exec"
	"github.com/gosuda/dalgona/internal/tool/patch"
	"github.com/gosuda/dalgona/internal/tool/read"
	"github.com/gosuda/dalgona/internal/tool/search"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	os.Exit(m.Run())
}

type response struct {
	status int
	body   string
	gate   chan struct{}
}

type fakeModel struct {
	t        *testing.T
	server   *httptest.Server
	mu       sync.Mutex
	queue    []response
	requests [][]byte
	models   []string
	arrived  chan int
	route    func(body []byte) (response, bool)
}

func newFakeModel(t *testing.T) *fakeModel {
	t.Helper()
	f := &fakeModel{t: t, models: []string{"gpt-test"}, arrived: make(chan int, 64)}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /responses", f.respond)
	mux.HandleFunc("GET /models", f.listModels)
	f.server = httptest.NewServer(mux)
	t.Cleanup(f.server.Close)
	return f
}

func (f *fakeModel) push(responses ...response) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.queue = append(f.queue, responses...)
}

func (f *fakeModel) respond(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	next, count := f.record(body)
	select {
	case f.arrived <- count:
	default:
	}
	if next.gate != nil {
		select {
		case <-next.gate:
		case <-r.Context().Done():
			return
		}
	}
	if next.status != 0 {
		w.WriteHeader(next.status)
		_, _ = io.WriteString(w, next.body)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = io.WriteString(w, next.body)
}

func (f *fakeModel) record(body []byte) (response, int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.requests = append(f.requests, body)
	if f.route != nil {
		if routed, ok := f.route(body); ok {
			return routed, len(f.requests)
		}
	}
	if len(f.queue) == 0 {
		return response{body: sseText("done")}, len(f.requests)
	}
	next := f.queue[0]
	f.queue = f.queue[1:]
	return next, len(f.requests)
}

func (f *fakeModel) listModels(w http.ResponseWriter, _ *http.Request) {
	f.mu.Lock()
	defer f.mu.Unlock()
	items := make([]string, len(f.models))
	for i, id := range f.models {
		items[i] = `{"id":"` + id + `"}`
	}
	_, _ = io.WriteString(w, `{"data":[`+strings.Join(items, ",")+`]}`)
}

func (f *fakeModel) request(i int) map[string]any {
	f.t.Helper()
	f.mu.Lock()
	body := f.requests[i]
	f.mu.Unlock()
	var decoded map[string]any
	if err := json.Unmarshal(body, &decoded); err != nil {
		f.t.Fatalf("decode request %d: %v", i, err)
	}
	return decoded
}

func (f *fakeModel) raw(i int) string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return string(f.requests[i])
}

func (f *fakeModel) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.requests)
}

func sseEvent(kind, data string) string {
	return "event: " + kind + "\ndata: " + data + "\n\n"
}

func completed() string {
	return sseEvent("response.completed", `{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":20,"output_tokens":7}}}`)
}

func sseText(body string) string {
	delta := `{"type":"response.output_text.delta","output_index":0,"delta":` + string(jsonString(body)) + `}`
	return sseEvent("response.output_text.delta", delta) + completed()
}

type call struct {
	id   string
	name string
	args string
}

func sseCalls(calls ...call) string {
	var out strings.Builder
	for i, item := range calls {
		index := fmt.Sprint(i + 1)
		head := `{"type":"function_call","call_id":"` + item.id + `","name":"` + item.name + `","arguments":`
		out.WriteString(sseEvent("response.output_item.added",
			`{"type":"response.output_item.added","output_index":`+index+`,"item":`+head+`""}}`))
		out.WriteString(sseEvent("response.output_item.done",
			`{"type":"response.output_item.done","output_index":`+index+`,"item":`+head+string(jsonString(item.args))+`}}`))
	}
	out.WriteString(completed())
	return out.String()
}

func text(body string) response { return response{body: sseText(body)} }

func calls(c ...call) response { return response{body: sseCalls(c...)} }

func failing(status int, body string) response { return response{status: status, body: body} }

type logBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (l *logBuffer) Write(p []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.buf.Write(p)
}

func (l *logBuffer) String() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.buf.String()
}

type harness struct {
	t       *testing.T
	home    string
	cwd     string
	journal *journal.Store
	model   *fakeModel
	cfg     HostConfig
	logs    *logBuffer
	host    *Host
}

func newHarness(t *testing.T, configure ...func(*harness)) *harness {
	t.Helper()
	h := &harness{t: t, home: t.TempDir(), cwd: t.TempDir(), model: newFakeModel(t), logs: &logBuffer{}}
	t.Cleanup(h.reapProcesses)
	if resolved, err := filepath.EvalSymlinks(h.cwd); err == nil {
		h.cwd = resolved
	}
	store, err := journal.Open(h.home)
	if err != nil {
		t.Fatal(err)
	}
	h.journal = store
	t.Cleanup(func() { _ = store.Close() })
	h.cfg = HostConfig{
		Journal: store,
		Config: config.Config{
			Home: h.home, Cwd: h.cwd, Model: "openai/gpt-test", Approval: config.ApprovalAuto,
			Sandbox: config.SandboxOff, Product: config.ProductDalgona, Reasoning: config.ReasoningDefault,
		},
		Providers: h.providers,
		Tools:     h.tools,
		Runner:    h.runner,
		Logger:    slog.New(slog.NewTextHandler(h.logs, nil)),
	}
	for _, apply := range configure {
		apply(h)
	}
	h.host = h.newHost()
	return h
}

func (h *harness) newHost() *Host {
	h.t.Helper()
	host, err := NewHost(h.cfg)
	if err != nil {
		h.t.Fatal(err)
	}
	h.t.Cleanup(func() { _ = host.Close() })
	return host
}

func (h *harness) providers(onRetry RetryFunc) (*model.Registry, error) {
	registry := model.NewRegistry()
	retry := model.DefaultRetryOptions()
	retry.Sleep = func(context.Context, time.Duration) error { return nil }
	retry.OnRetry = onRetry
	provider := openai.New(openai.Config{
		BaseURL: h.model.server.URL, Client: h.model.server.Client(), Retry: retry,
		APIKey: func(context.Context) (string, error) { return "test-key", nil },
	})
	return registry, registry.Register("openai", provider)
}

func (h *harness) tools(context.Context, string, string) ([]tool.Tool, error) {
	return []tool.Tool{read.New(), search.New(), patch.New("replace"), exec.New()}, nil
}

func (h *harness) runner(_ context.Context, id, _ string) (runner.Runner, error) {
	return runner.NewLocal(filepath.Join(h.home, "sessions", id, "runs"))
}

func (h *harness) reapProcesses() {
	runs, err := filepath.Glob(filepath.Join(h.home, "sessions", "*", "runs", "*"))
	if err != nil {
		h.t.Error(err)
		return
	}
	for _, run := range runs {
		local, err := runner.NewLocal(filepath.Dir(run))
		if err != nil {
			h.t.Error(err)
			continue
		}
		if !killUntilExit(local, filepath.Base(run)) {
			h.t.Errorf("process %s outlived its test", run)
		}
	}
}

func killUntilExit(local *runner.Local, id string) bool {
	ctx := context.Background()
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(20 * time.Millisecond) {
		out, err := local.Read(ctx, id, 0)
		if errors.Is(err, runner.ErrProcessNotFound) || (err == nil && out.Exit != nil) {
			return true
		}
		_ = local.Signal(ctx, id, runner.SignalKill)
	}
	return false
}

func (h *harness) start() *Thread {
	h.t.Helper()
	thread, err := h.host.Start(h.t.Context(), ThreadSpec{})
	if err != nil {
		h.t.Fatal(err)
	}
	return thread
}

func (h *harness) submit(thread *Thread, prompt string) {
	h.t.Helper()
	if err := thread.Submit(h.t.Context(), Input{Kind: InputPrompt, Text: prompt}); err != nil {
		h.t.Fatal(err)
	}
}

func (h *harness) waitIdle(thread *Thread) {
	h.t.Helper()
	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		status, err := thread.Status()
		if err != nil {
			h.t.Fatal(err)
		}
		if !status.Busy {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	h.t.Fatalf("thread %s did not become idle; events: %v", thread.ID(), h.types(thread.ID()))
}

func (h *harness) waitRequests(n int) {
	h.t.Helper()
	deadline := time.After(60 * time.Second)
	for h.model.count() < n {
		select {
		case <-h.model.arrived:
		case <-deadline:
			h.t.Fatalf("waited for %d model requests, got %d", n, h.model.count())
		}
	}
}

func (h *harness) events(id string) []event.Seqd {
	h.t.Helper()
	var out []event.Seqd
	err := h.journal.Replay(id, func(seq uint64, raw []byte) error {
		decoded, err := event.Decode(raw)
		out = append(out, event.Seqd{Seq: seq, Event: decoded})
		return err
	})
	if err != nil {
		h.t.Fatal(err)
	}
	return out
}

func (h *harness) types(id string) []string {
	h.t.Helper()
	events := h.events(id)
	out := make([]string, len(events))
	for i, item := range events {
		out[i] = item.Event.Type()
	}
	return out
}

func (h *harness) journalBytes(id string) string {
	h.t.Helper()
	data, err := os.ReadFile(filepath.Join(h.home, "sessions", id, "journal.jsonl"))
	if err != nil {
		h.t.Fatal(err)
	}
	return string(data)
}

func (h *harness) write(rel, content string) {
	h.t.Helper()
	path := filepath.Join(h.cwd, rel)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		h.t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		h.t.Fatal(err)
	}
}

func completedOutputs(events []event.Seqd) map[string]event.ToolCompleted {
	out := make(map[string]event.ToolCompleted)
	for _, item := range events {
		if value, ok := item.Event.(event.ToolCompleted); ok {
			out[value.CallID] = value
		}
	}
	return out
}

func lastTurn(events []event.Seqd) event.TurnCompleted {
	for i := len(events) - 1; i >= 0; i-- {
		if value, ok := events[i].Event.(event.TurnCompleted); ok {
			return value
		}
	}
	return event.TurnCompleted{}
}

func inputItems(request map[string]any) []map[string]any {
	raw, _ := request["input"].([]any)
	out := make([]map[string]any, 0, len(raw))
	for _, item := range raw {
		if object, ok := item.(map[string]any); ok {
			out = append(out, object)
		}
	}
	return out
}

func userTexts(request map[string]any) []string {
	var out []string
	for _, item := range inputItems(request) {
		if item["role"] != "user" {
			continue
		}
		content, _ := item["content"].([]any)
		for _, part := range content {
			if object, ok := part.(map[string]any); ok {
				if value, ok := object["text"].(string); ok {
					out = append(out, value)
				}
			}
		}
	}
	return out
}

func callOutputs(request map[string]any) map[string]string {
	out := make(map[string]string)
	for _, item := range inputItems(request) {
		if item["type"] == "function_call_output" {
			id, _ := item["call_id"].(string)
			output, _ := item["output"].(string)
			out[id] = output
		}
	}
	return out
}
