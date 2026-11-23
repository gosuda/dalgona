package rpcserver

import (
	"context"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/anthropic"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/read"
)

func effortTurn(t *testing.T, c *wireClient, threadID, effort, text string) {
	t.Helper()
	params := map[string]any{"threadId": threadID, "input": []rpc.InputItem{{Type: "text", Text: text}}}
	if effort != "" {
		params["effort"] = effort
	}
	var response rpc.TurnStartResponse
	c.ok("turn/start", params, &response)
	expect(t, response.Turn.ID != "" && response.Turn.Status == rpc.StatusInProgress, "turn/start", response.Turn)
	log := readTurn(t, c.collect())
	expect(t, log.status == rpc.StatusCompleted, "turn status", log.status)
}

func TestThreadStartConfigEffortDefaultsTurns(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	requests := recordRequests(t, r)
	r.llm.push(llmReply{body: sseText("one")}, llmReply{body: sseText("two")}, llmReply{body: sseText("three")})
	c := r.connect()
	var started rpc.ThreadStartResponse
	c.ok("thread/start", map[string]any{
		"cwd": r.cwd, "model": "gpt-6.1-sol",
		"config": map[string]any{"model_reasoning_effort": "high"},
	}, &started)
	expect(t, started.ReasoningEffort == "high", "thread/start reasoningEffort", started.ReasoningEffort)
	c.await("thread/started", nil)
	threadID := started.Thread.ID
	effortTurn(t, c, threadID, "", "default effort")
	effortTurn(t, c, threadID, "low", "turn effort")
	seen := requests.seen()
	expect(t, len(seen) == 2 && seen[0].effort == "high" && seen[1].effort == "low", "model request efforts", seen)

	reopen(t, r)
	recovered := r.connect()
	var resumed rpc.ThreadStartResponse
	recovered.ok("thread/resume", map[string]any{"threadId": threadID}, &resumed)
	expect(t, resumed.ReasoningEffort == "high", "thread/resume reasoningEffort", resumed.ReasoningEffort)
	effortTurn(t, recovered, threadID, "", "after resume")
	seen = requests.seen()
	expect(t, len(seen) == 3 && seen[2].effort == "high", "default effort after resume", seen)
}

func TestActiveResumeRetainsResolvedEmptyEffort(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	gate := make(chan struct{})
	defer close(gate)
	r.llm.push(llmReply{body: sseText("done"), gate: gate})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	turnID := startTurn(t, c, threadID, "unknown effort capabilities")
	effort := c.await("dalgona/turn/effort", nil)
	expect(t, fieldOf(t, effort.Params, "threadId") == threadID && fieldOf(t, effort.Params, "turnId") == turnID && fieldOf(t, effort.Params, "effort") == "", "resolved empty effort", string(effort.Params))
	reconnected := r.connect()
	for _, exclude := range []bool{false, true} {
		var resumed rpc.ThreadResumeResponse
		reconnected.ok("thread/resume", rpc.ThreadResumeParams{ThreadID: threadID, ExcludeTurns: exclude}, &resumed)
		expect(t, resumed.TurnEffort != nil && *resumed.TurnEffort == "", "active resume authoritative empty effort", resumed.TurnEffort)
	}
	c.ok("turn/interrupt", rpc.TurnInterruptParams{ThreadID: threadID, TurnID: turnID}, nil)
	c.await("turn/completed", nil)
	var idle rpc.ThreadResumeResponse
	reconnected.ok("thread/resume", rpc.ThreadResumeParams{ThreadID: threadID}, &idle)
	expect(t, idle.TurnEffort == nil, "idle resume omits turnEffort", idle.TurnEffort)
}

func TestThreadStartRejectsUnknownConfigEffort(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	c.fails("thread/start", map[string]any{
		"cwd":    r.cwd,
		"config": map[string]any{"model_reasoning_effort": "loud"},
	}, rpc.CodeInvalidParams, "unsupported model_reasoning_effort loud")
}

func TestTurnStartRejectsUnknownEffortValue(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	c.fails("turn/start", map[string]any{
		"threadId": threadID,
		"input":    []textInput{{Type: "text", Text: "hi"}},
		"effort":   "loud",
	}, rpc.CodeInvalidParams, "unsupported model_reasoning_effort loud")
}

const declaredEffortModels = `{"data":[{"id":"claude-test","max_input_tokens":32000,"max_tokens":4096,"capabilities":{"effort":{"supported":true,"high":{"supported":true}}}}],"has_more":false}`

type fakeAnthropic struct {
	t      *testing.T
	models string
	server *httptest.Server
	mu     sync.Mutex
	bodies []string
}

func (f *fakeAnthropic) count() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return len(f.bodies)
}

func (f *fakeAnthropic) body(i int) string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.bodies[i]
}

func anthropicTurnSSE() string {
	return strings.Join([]string{
		`event: message_start`,
		`data: {"type":"message_start","message":{"id":"m1","model":"claude-test","usage":{"input_tokens":20,"output_tokens":1}}}`,
		``,
		`event: content_block_delta`,
		`data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}`,
		``,
		`event: message_delta`,
		`data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}`,
		``,
		`event: message_stop`,
		`data: {"type":"message_stop"}`,
		``,
	}, "\n")
}

func newAnthropicRig(t *testing.T, models string) (*rig, *fakeAnthropic) {
	t.Helper()
	f := &fakeAnthropic{t: t, models: models}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, f.models)
	})
	mux.HandleFunc("POST /messages", func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		f.mu.Lock()
		f.bodies = append(f.bodies, string(body))
		f.mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, anthropicTurnSSE())
	})
	f.server = httptest.NewServer(mux)
	t.Cleanup(f.server.Close)
	r := &rig{t: t, home: t.TempDir(), cwd: t.TempDir()}
	t.Setenv("DAL_HOME", r.home)
	if resolved, err := filepath.EvalSymlinks(r.cwd); err == nil {
		r.cwd = resolved
	}
	store, err := journal.Open(r.home)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	credentials, err := auth.Load(r.home)
	if err != nil {
		t.Fatal(err)
	}
	r.cfg = agent.HostConfig{
		Journal: store, Auth: credentials, ProviderIDs: []string{"anthropic"},
		Config: config.Config{
			Home: r.home, Cwd: r.cwd, Model: "anthropic/claude-test", Approval: config.ApprovalYolo,
			Sandbox: config.SandboxOff, Product: config.ProductDal, Reasoning: config.ReasoningDefault,
		},
		Providers: func(onRetry agent.RetryFunc) (*model.Registry, error) {
			registry := model.NewRegistry()
			retry := model.DefaultRetryOptions()
			retry.Sleep = func(context.Context, time.Duration) error { return nil }
			retry.OnRetry = onRetry
			return registry, registry.Register("anthropic", anthropic.New(anthropic.Config{
				BaseURL: f.server.URL, Client: f.server.Client(), Retry: retry,
				APIKey: func(context.Context) (string, error) { return "test-key", nil },
			}))
		},
		Tools: func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{read.New()}, nil
		},
		Runner: func(_ context.Context, id, _ string) (runner.Runner, error) {
			return runner.NewLocal(filepath.Join(r.home, "sessions", id, "runs"))
		},
		Logger: slog.New(slog.DiscardHandler),
	}
	r.openHost()
	return r, f
}

func TestTurnStartRefusesEffortOutsideDeclaredLevels(t *testing.T) {
	r, f := newAnthropicRig(t, declaredEffortModels)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	c.fails("turn/start", map[string]any{
		"threadId": threadID,
		"input":    []textInput{{Type: "text", Text: "hi"}},
		"effort":   "xhigh",
	}, rpc.CodeInvalidParams, "unsupported reasoning effort: xhigh")
	expect(t, f.count() == 0, "model requests after the refused turn/start", f.count())
}

func TestTurnStartSendsDeclaredEffortAndOmitsItForUnknownModels(t *testing.T) {
	r, f := newAnthropicRig(t, declaredEffortModels)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	effortTurn(t, c, threadID, "high", "declared effort turn")
	expect(t, strings.Contains(f.body(0), `"thinking"`), "declared effort request body", f.body(0))
	var started rpc.ThreadStartResponse
	c.ok("thread/start", map[string]any{"cwd": r.cwd, "model": "claude-unknown"}, &started)
	c.await("thread/started", nil)
	effortTurn(t, c, started.Thread.ID, "xhigh", "unknown model effort turn")
	expect(t, !strings.Contains(f.body(1), `"thinking"`), "unknown model request body", f.body(1))
	expect(t, !strings.Contains(f.body(1), "xhigh"), "unknown model request body", f.body(1))
}

func TestConfigWriteRefusesEffortOutsideDeclaredLevels(t *testing.T) {
	r, _ := newAnthropicRig(t, declaredEffortModels)
	c := r.connect()
	c.fails("config/value/write", map[string]any{"keyPath": "model_reasoning_effort", "value": "xhigh"}, rpc.CodeInvalidParams, "unsupported reasoning effort: xhigh")
	var response rpc.ConfigWriteResponse
	c.ok("config/value/write", map[string]any{"keyPath": "model_reasoning_effort", "value": "high"}, &response)
}

const mixedEffortModels = `{"data":[` +
	`{"id":"claude-test","max_input_tokens":32000,"max_tokens":4096,"capabilities":{"effort":{"supported":true,"high":{"supported":true}}}},` +
	`{"id":"claude-plain","max_input_tokens":32000,"max_tokens":4096}` +
	`],"has_more":false}`

func threadCount(t *testing.T, c *wireClient) int {
	t.Helper()
	var list rpc.ThreadListResponse
	c.ok("thread/list", map[string]any{}, &list)
	return len(list.Data)
}

func effortStart(r *rig, modelID, effort string) map[string]any {
	params := map[string]any{"cwd": r.cwd, "config": map[string]any{"model_reasoning_effort": effort}}
	if modelID != "" {
		params["model"] = modelID
	}
	return params
}

func TestThreadStartRefusesEffortOutsideDeclaredLevels(t *testing.T) {
	r, f := newAnthropicRig(t, mixedEffortModels)
	c := r.connect()
	c.fails("thread/start", effortStart(r, "", "medium"), rpc.CodeInvalidParams, "unsupported reasoning effort: medium")
	c.fails("thread/start", effortStart(r, "claude-test", "medium"), rpc.CodeInvalidParams, "unsupported reasoning effort: medium")
	expect(t, threadCount(t, c) == 0, "threads after the refused thread/start", threadCount(t, c))
	expect(t, f.count() == 0, "model requests after the refused thread/start", f.count())
	var started rpc.ThreadStartResponse
	c.ok("thread/start", effortStart(r, "", "high"), &started)
	c.await("thread/started", nil)
	expect(t, started.ReasoningEffort == "high", "thread/start reasoningEffort", started.ReasoningEffort)
	expect(t, threadCount(t, c) == 1, "threads after the accepted thread/start", threadCount(t, c))
}

func TestThreadStartAcceptsEffortForModelWithoutDeclaredLevels(t *testing.T) {
	r, f := newAnthropicRig(t, mixedEffortModels)
	c := r.connect()
	var started rpc.ThreadStartResponse
	c.ok("thread/start", effortStart(r, "claude-plain", "medium"), &started)
	c.await("thread/started", nil)
	expect(t, started.ReasoningEffort == "medium", "thread/start reasoningEffort", started.ReasoningEffort)
	effortTurn(t, c, started.Thread.ID, "", "plain model turn")
	expect(t, !strings.Contains(f.body(0), `"thinking"`), "plain model request body", f.body(0))
	expect(t, !strings.Contains(f.body(0), `"output_config"`), "plain model request body", f.body(0))
}
