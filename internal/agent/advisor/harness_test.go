package advisor

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"io"
	"io/fs"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/openai"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/read"
	"github.com/gosuda/dalgona/internal/tool/search"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	os.Exit(m.Run())
}

const (
	rolePrimary = "primary"
	roleAdvisor = "advisor"
	waitLimit   = 20 * time.Second
)

type response struct {
	status int
	body   string
	gate   chan struct{}
}

type fakeModel struct {
	t        *testing.T
	server   *httptest.Server
	mu       sync.Mutex
	queues   map[string][]response
	fallback map[string]response
	requests map[string][]string
	changed  chan struct{}
	models   []string
}

func newFakeModel(t *testing.T) *fakeModel {
	t.Helper()
	f := &fakeModel{
		t: t, queues: map[string][]response{}, requests: map[string][]string{}, changed: make(chan struct{}),
		fallback: map[string]response{rolePrimary: text("done"), roleAdvisor: text("nothing to add")},
		models:   []string{"gpt-test"},
	}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /responses", f.respond)
	mux.HandleFunc("GET /models", f.listModels)
	f.server = httptest.NewServer(mux)
	t.Cleanup(f.server.Close)
	return f
}

func (f *fakeModel) push(role string, responses ...response) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.queues[role] = append(f.queues[role], responses...)
}

func (f *fakeModel) setFallback(role string, next response) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.fallback[role] = next
}

func (f *fakeModel) respond(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	role := rolePrimary
	if bytes.Contains(body, []byte(`"name":"advise"`)) {
		role = roleAdvisor
	}
	f.mu.Lock()
	f.requests[role] = append(f.requests[role], string(body))
	next := f.fallback[role]
	if queue := f.queues[role]; len(queue) > 0 {
		next, f.queues[role] = queue[0], queue[1:]
	}
	close(f.changed)
	f.changed = make(chan struct{})
	f.mu.Unlock()
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

func (f *fakeModel) listModels(w http.ResponseWriter, _ *http.Request) {
	f.mu.Lock()
	defer f.mu.Unlock()
	items := make([]string, len(f.models))
	for i, id := range f.models {
		items[i] = `{"id":"` + id + `"}`
	}
	_, _ = io.WriteString(w, `{"data":[`+strings.Join(items, ",")+`]}`)
}

func (f *fakeModel) count(role string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.requests[role])
}

func (f *fakeModel) waitRequests(role string, n int) {
	f.t.Helper()
	deadline := time.After(waitLimit)
	for {
		f.mu.Lock()
		count, changed := len(f.requests[role]), f.changed
		f.mu.Unlock()
		if count >= n {
			return
		}
		select {
		case <-changed:
		case <-deadline:
			f.t.Fatalf("waited for %d %s requests, got %d", n, role, count)
		}
	}
}

func (f *fakeModel) request(role string, i int) map[string]any {
	f.t.Helper()
	f.mu.Lock()
	body := f.requests[role][i]
	f.mu.Unlock()
	var decoded map[string]any
	if err := json.Unmarshal([]byte(body), &decoded); err != nil {
		f.t.Fatalf("decode %s request %d: %v", role, i, err)
	}
	return decoded
}

func quote(text string) string {
	out, err := jsontext.AppendQuote(nil, text)
	if err != nil {
		panic("test literal is not valid UTF-8: " + err.Error())
	}
	return string(out)
}

func sseEvent(kind, data string) string {
	return "event: " + kind + "\ndata: " + data + "\n\n"
}

type call struct {
	id   string
	name string
	args string
}

func reply(body string, calls ...call) response {
	var out strings.Builder
	if body != "" {
		out.WriteString(sseEvent("response.output_text.delta",
			`{"type":"response.output_text.delta","output_index":0,"delta":`+quote(body)+`}`))
	}
	for i, item := range calls {
		index := strconv.Itoa(i + 1)
		head := `{"type":"function_call","call_id":"` + item.id + `","name":"` + item.name + `","arguments":`
		out.WriteString(sseEvent("response.output_item.added",
			`{"type":"response.output_item.added","output_index":`+index+`,"item":`+head+`""}}`))
		out.WriteString(sseEvent("response.output_item.done",
			`{"type":"response.output_item.done","output_index":`+index+`,"item":`+head+quote(item.args)+`}}`))
	}
	out.WriteString(sseEvent("response.completed",
		`{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":20,"output_tokens":7}}}`))
	return response{body: out.String()}
}

func text(body string) response { return reply(body) }

func calls(c ...call) response { return reply("", c...) }

func gated(r response, gate chan struct{}) response {
	r.gate = gate
	return r
}

func advise(id, note, severity string) call {
	return call{id: id, name: "advise", args: `{"note":` + quote(note) + `,"severity":"` + severity + `"}`}
}

type harness struct {
	t     *testing.T
	home  string
	cwd   string
	model *fakeModel
	cfg   config.Config
	store *journal.Store
	host  *agent.Host
}

func newHarness(t *testing.T, configure func(*config.Config)) *harness {
	t.Helper()
	h := &harness{t: t, home: t.TempDir(), cwd: t.TempDir(), model: newFakeModel(t)}
	if resolved, err := filepath.EvalSymlinks(h.cwd); err == nil {
		h.cwd = resolved
	}
	h.cfg = config.Config{
		Home: h.home, Cwd: h.cwd, Model: "openai/gpt-test", Approval: config.ApprovalAuto,
		Sandbox: config.SandboxOff, Product: config.ProductDalgona, Reasoning: config.ReasoningDefault,
		CompactionThreshold: .8, CompactionKeepTokens: 20000,
		Advisor: config.AdvisorConfig{
			Enabled: true, Review: config.AdvisorReviewTurn, Interval: 1, Sync: config.AdvisorSyncOff,
			ContinueSeverity: "blocker", ImmuneTurns: 3, MaxNotes: 3, Tools: []string{"read", "search"},
		},
	}
	if configure != nil {
		configure(&h.cfg)
	}
	if h.cfg.Advisors == nil {
		h.cfg.Advisors = map[string]config.AdvisorSpec{config.DefaultAdvisorName: {
			Review: h.cfg.Advisor.Review, Interval: h.cfg.Advisor.Interval, Sync: h.cfg.Advisor.Sync,
			Tools: h.cfg.Advisor.Tools, Enabled: true,
		}}
	}
	h.store, h.host = h.open(h.home)
	return h
}

func (h *harness) open(home string) (*journal.Store, *agent.Host) {
	h.t.Helper()
	store, err := journal.Open(home)
	if err != nil {
		h.t.Fatal(err)
	}
	cfg := h.cfg
	cfg.Home = home
	host, err := agent.NewHost(agent.HostConfig{
		Journal: store, Config: cfg, Providers: h.providers, Tools: h.tools,
		Runner: func(_ context.Context, id, _ string) (runner.Runner, error) {
			return runner.NewLocal(filepath.Join(home, "sessions", id, "runs"))
		},
		Logger: slog.New(slog.DiscardHandler),
	})
	if err != nil {
		h.t.Fatal(err)
	}
	h.t.Cleanup(func() {
		_ = host.Close()
		_ = store.Close()
	})
	return store, host
}

func (h *harness) providers(onRetry agent.RetryFunc) (*model.Registry, error) {
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

func (*harness) tools(context.Context, string, string) ([]tool.Tool, error) {
	return []tool.Tool{read.New(), search.New()}, nil
}

func (h *harness) start() (*agent.Thread, *Runtime) {
	h.t.Helper()
	thread, err := h.host.Start(h.t.Context(), agent.ThreadSpec{})
	if err != nil {
		h.t.Fatal(err)
	}
	return thread, h.attach(h.host, thread)
}

func (h *harness) attach(host *agent.Host, thread *agent.Thread) *Runtime {
	h.t.Helper()
	rt, err := Attach(h.t.Context(), host, thread, h.cfg)
	if err != nil {
		h.t.Fatal(err)
	}
	h.t.Cleanup(func() { _ = rt.Close() })
	return rt
}

func (h *harness) submit(thread *agent.Thread, prompt string) {
	h.t.Helper()
	if err := thread.Submit(h.t.Context(), agent.Input{Kind: agent.InputPrompt, Text: prompt}); err != nil {
		h.t.Fatal(err)
	}
}

func (h *harness) waitFor(what string, done func() bool) {
	h.t.Helper()
	deadline := time.Now().Add(waitLimit)
	for !done() {
		if time.Now().After(deadline) {
			h.t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func (h *harness) waitIdle(thread *agent.Thread) {
	h.t.Helper()
	h.waitFor("thread "+thread.ID()+" to become idle", func() bool {
		status, err := thread.Status()
		return err == nil && !status.Busy
	})
}

func (h *harness) write(rel, content string) {
	h.t.Helper()
	if err := os.WriteFile(filepath.Join(h.cwd, rel), []byte(content), 0o644); err != nil {
		h.t.Fatal(err)
	}
}

func journalEvents(t *testing.T, store *journal.Store, id string) []event.Seqd {
	t.Helper()
	var out []event.Seqd
	err := store.Replay(id, func(seq uint64, raw []byte) error {
		decoded, err := event.Decode(raw)
		out = append(out, event.Seqd{Seq: seq, Event: decoded})
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return out
}

func notesIn(events []event.Seqd) []event.AdvisorNote {
	var out []event.AdvisorNote
	for _, item := range events {
		if note, ok := item.Event.(event.AdvisorNote); ok {
			out = append(out, note)
		}
	}
	return out
}

func turnEnds(events []event.Seqd) []event.TurnCompleted {
	var out []event.TurnCompleted
	for _, item := range events {
		if value, ok := item.Event.(event.TurnCompleted); ok {
			out = append(out, value)
		}
	}
	return out
}

func toolOutputs(events []event.Seqd) map[string]string {
	out := map[string]string{}
	for _, item := range events {
		if value, ok := item.Event.(event.ToolCompleted); ok {
			out[value.CallID] = value.Output
		}
	}
	return out
}

func userTexts(request map[string]any) []string {
	var out []string
	items, _ := request["input"].([]any)
	for _, raw := range items {
		item, _ := raw.(map[string]any)
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

func childOf(t *testing.T, host *agent.Host, parent string) string {
	t.Helper()
	id, ok := host.AdvisorChild(parent, config.DefaultAdvisorName)
	if !ok {
		t.Fatalf("thread %s has no advisor session", parent)
	}
	return id
}

func copyRegularFiles(t *testing.T, from, to string) {
	t.Helper()
	source, err := os.OpenRoot(from)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = source.Close() }()
	err = fs.WalkDir(source.FS(), ".", func(rel string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		target := filepath.Join(to, rel)
		if entry.IsDir() {
			return os.MkdirAll(target, 0o755)
		}
		if !entry.Type().IsRegular() || strings.HasSuffix(rel, ".lock") {
			return nil
		}
		data, err := source.ReadFile(rel)
		if err != nil {
			return err
		}
		return os.WriteFile(target, data, 0o644)
	})
	if err != nil {
		t.Fatal(err)
	}
}
