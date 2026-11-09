package scripthost_test

import (
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"io"
	"io/fs"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
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
	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/scripthost"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/exec"
	"github.com/gosuda/dalgona/internal/tool/read"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	os.Exit(m.Run())
}

type reply struct {
	body string
	gate <-chan struct{}
}

func frame(kind, data string) string { return "event: " + kind + "\ndata: " + data + "\n\n" }

func quoted(text string) string {
	out, _ := jsontext.AppendQuote(nil, text)
	return string(out)
}

const done = `{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":5,"output_tokens":2}}}`

func text(body string) reply {
	delta := `{"type":"response.output_text.delta","output_index":0,"delta":` + quoted(body) + `}`
	return reply{body: frame("response.output_text.delta", delta) + frame("response.completed", done)}
}

func toolCall(id, name, args string) reply {
	item := `{"type":"function_call","call_id":"` + id + `","name":"` + name + `","arguments":` + quoted(args) + `}`
	return reply{body: frame("response.output_item.done", `{"type":"response.output_item.done","output_index":1,"item":`+item+`}`) + frame("response.completed", done)}
}

func evalCall(code string) reply {
	args, _ := jsontext.AppendQuote([]byte(`{"code":`), code)
	return toolCall("e", "eval", string(append(args, '}')))
}

type fakeModel struct {
	mu     sync.Mutex
	queue  []reply
	routes map[string][]reply
	seen   map[string]int
	server *httptest.Server
}

func (f *fakeModel) push(replies ...reply) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.queue = append(f.queue, replies...)
}

func (f *fakeModel) requests(prompt string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.seen[prompt]
}

func (f *fakeModel) next(body []byte) reply {
	prompt := firstPrompt(body)
	f.mu.Lock()
	defer f.mu.Unlock()
	if routed, ok := f.routes[prompt]; ok {
		index := min(f.seen[prompt], len(routed)-1)
		f.seen[prompt]++
		return routed[index]
	}
	if len(f.queue) == 0 {
		return text("done")
	}
	head := f.queue[0]
	f.queue = f.queue[1:]
	return head
}

func (f *fakeModel) respond(w http.ResponseWriter, r *http.Request) {
	body, _ := io.ReadAll(r.Body)
	next := f.next(body)
	if next.gate != nil {
		select {
		case <-next.gate:
		case <-r.Context().Done():
			return
		}
	}
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = io.WriteString(w, next.body)
}

func firstPrompt(body []byte) string {
	var request struct {
		Input []struct {
			Role    string `json:"role"`
			Content []struct {
				Text string `json:"text"`
			} `json:"content"`
		} `json:"input"`
	}
	if json.Unmarshal(body, &request) != nil {
		return ""
	}
	for _, item := range request.Input {
		if item.Role == "user" && len(item.Content) > 0 {
			return item.Content[0].Text
		}
	}
	return ""
}

type harness struct {
	t       *testing.T
	home    string
	cwd     string
	store   *journal.Store
	model   *fakeModel
	scripts scripthost.Config
	cfg     agent.HostConfig
	host    *agent.Host
}

func newHarness(t *testing.T, configure ...func(*harness)) *harness {
	t.Helper()
	h := &harness{t: t, home: t.TempDir(), cwd: t.TempDir(), model: &fakeModel{routes: map[string][]reply{}, seen: map[string]int{}}}
	created := h.home
	t.Cleanup(func() { reapProcesses(t, created) })
	mux := http.NewServeMux()
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`)
	})
	mux.HandleFunc("POST /responses", h.model.respond)
	h.model.server = httptest.NewServer(mux)
	t.Cleanup(h.model.server.Close)
	h.scripts = scripthost.Config{Mode: config.ToolModeEvalOnly, Logger: slog.New(slog.DiscardHandler)}
	h.cfg = agent.HostConfig{
		Config: config.Config{
			Home: h.home, Cwd: h.cwd, Model: "openai/gpt-test", Approval: config.ApprovalYolo,
			Sandbox: config.SandboxOff, Product: config.ProductDalgona, Reasoning: config.ReasoningDefault,
		},
		Providers: h.providers,
		Tools: func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{read.New(), exec.New()}, nil
		},
		Runner: func(_ context.Context, id, _ string) (runner.Runner, error) {
			return runner.NewLocal(filepath.Join(h.home, "sessions", id, "runs"))
		},
		Logger: slog.New(slog.DiscardHandler),
	}
	for _, apply := range configure {
		apply(h)
	}
	h.open(h.home)
	return h
}

func (h *harness) open(home string) {
	h.t.Helper()
	store, err := journal.Open(home)
	if err != nil {
		h.t.Fatal(err)
	}
	h.t.Cleanup(func() { _ = store.Close() })
	h.home, h.store = home, store
	h.cfg.Config.Home, h.cfg.Journal = home, store
	h.cfg.Extension = scripthost.New(h.scripts)
	host, err := agent.NewHost(h.cfg)
	if err != nil {
		h.t.Fatal(err)
	}
	h.t.Cleanup(func() { _ = host.Close() })
	h.host = host
}

func reapProcesses(t *testing.T, home string) {
	runs, err := filepath.Glob(filepath.Join(home, "sessions", "*", "runs", "*"))
	if err != nil {
		t.Error(err)
		return
	}
	for _, run := range runs {
		local, err := runner.NewLocal(filepath.Dir(run))
		if err != nil {
			t.Error(err)
			continue
		}
		if !killUntilExit(local, filepath.Base(run)) {
			t.Errorf("process %s outlived its test", run)
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

func (h *harness) providers(agent.RetryFunc) (*model.Registry, error) {
	retry := model.DefaultRetryOptions()
	retry.Sleep = func(context.Context, time.Duration) error { return nil }
	registry := model.NewRegistry()
	provider := openai.New(openai.Config{
		BaseURL: h.model.server.URL, Client: h.model.server.Client(), Retry: retry,
		APIKey: func(context.Context) (string, error) { return "key", nil },
	})
	return registry, registry.Register("openai", provider)
}

func (h *harness) run(prompt string) *agent.Thread {
	h.t.Helper()
	thread, err := h.host.Start(h.t.Context(), agent.ThreadSpec{})
	if err != nil {
		h.t.Fatal(err)
	}
	if err := thread.Submit(h.t.Context(), agent.Input{Kind: agent.InputPrompt, Text: prompt}); err != nil {
		h.t.Fatal(err)
	}
	return thread
}

func (h *harness) idle(thread *agent.Thread) []event.Seqd {
	h.t.Helper()
	for deadline := time.Now().Add(20 * time.Second); time.Now().Before(deadline); time.Sleep(5 * time.Millisecond) {
		status, err := thread.Status()
		if err != nil {
			h.t.Fatal(err)
		}
		if !status.Busy {
			return h.events(thread.ID())
		}
	}
	h.t.Fatalf("thread %s stayed busy", thread.ID())
	return nil
}

func (h *harness) events(id string) []event.Seqd {
	h.t.Helper()
	var out []event.Seqd
	err := h.store.Replay(id, func(seq uint64, raw []byte) error {
		decoded, err := event.Decode(raw)
		out = append(out, event.Seqd{Seq: seq, Event: decoded})
		return err
	})
	if err != nil {
		h.t.Fatal(err)
	}
	return out
}

func (h *harness) await(id string, match func([]event.Seqd) bool) {
	h.t.Helper()
	for deadline := time.Now().Add(20 * time.Second); time.Now().Before(deadline); time.Sleep(5 * time.Millisecond) {
		if match(h.events(id)) {
			return
		}
	}
	h.t.Fatalf("thread %s never reached the expected state", id)
}

func outputs(events []event.Seqd) map[string]event.ToolCompleted {
	out := make(map[string]event.ToolCompleted)
	for _, item := range events {
		if value, ok := item.Event.(event.ToolCompleted); ok {
			out[value.CallID] = value
		}
	}
	return out
}

func count(events []event.Seqd, kind string) int {
	n := 0
	for _, item := range events {
		if item.Event.Type() == kind {
			n++
		}
	}
	return n
}

func decisions(events []event.Seqd) map[string]event.ApprovalResolved {
	calls := make(map[string]string)
	out := make(map[string]event.ApprovalResolved)
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.ApprovalRequested:
			calls[value.RequestID] = value.CallID
		case event.ApprovalResolved:
			out[calls[value.RequestID]] = value
		}
	}
	return out
}

func loadPlugins(t *testing.T, files map[string]string) *script.Plugins {
	t.Helper()
	home := t.TempDir()
	if err := os.MkdirAll(filepath.Join(home, "plugins"), 0o700); err != nil {
		t.Fatal(err)
	}
	for name, source := range files {
		if err := os.WriteFile(filepath.Join(home, "plugins", name), []byte(source), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	plugins, err := script.LoadPlugins(home)
	if err != nil {
		t.Fatal(err)
	}
	return plugins
}

func copyHome(t *testing.T, from, to string) {
	t.Helper()
	source, err := os.OpenRoot(from)
	if err != nil {
		t.Fatal(err)
	}
	defer source.Close()
	err = fs.WalkDir(source.FS(), ".", func(rel string, entry fs.DirEntry, err error) error {
		target := filepath.Join(to, rel)
		switch {
		case err != nil:
			return err
		case entry.IsDir():
			return os.MkdirAll(target, 0o700)
		case strings.HasPrefix(entry.Name(), "state.db") || entry.Name() == "lock" || strings.Contains(rel, "runs"):
			return nil
		}
		data, err := source.ReadFile(rel)
		if err != nil {
			return err
		}
		return os.WriteFile(target, data, 0o600)
	})
	if err != nil {
		t.Fatal(err)
	}
}

func yield(id, data string) reply { return toolCall(id, "yield", `{"data":`+data+`}`) }
