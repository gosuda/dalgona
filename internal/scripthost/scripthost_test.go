package scripthost_test

import (
	"bytes"
	"context"
	"encoding/json/v2"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/scripthost"
)

const productDefaultDecisionsModel = "typesafe/jev-1.14"

type judgeServer struct {
	server   *httptest.Server
	requests atomic.Int32
}

func newJudgeServer(t *testing.T, hold <-chan struct{}) *judgeServer {
	t.Helper()
	s := &judgeServer{}
	s.server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		if r.Method != http.MethodPost || len(body) == 0 {
			http.Error(w, "judge ask must be a POST with a payload", http.StatusBadRequest)
			return
		}
		s.requests.Add(1)
		if hold != nil && bytes.Contains(body, []byte("hold")) {
			select {
			case <-hold:
			case <-r.Context().Done():
				return
			}
		}
		_, _ = io.WriteString(w, `{"answers":{"ok":{"type":"noul","noul":0.9}}}`)
	}))
	t.Cleanup(s.server.Close)
	return s
}

func (s *judgeServer) client(_ context.Context, modelName, _ string) (decide.Client, error) {
	if modelName == "" {
		modelName = productDefaultDecisionsModel
	}
	return decide.NewDecisions(decide.DecisionsConfig{
		BaseURL: s.server.URL, Model: modelName, HTTPClient: s.server.Client(),
		Token: func(context.Context) (string, error) { return "token", nil },
	}), nil
}

func withJudge(judge *judgeServer) func(*harness) {
	return func(h *harness) { h.scripts.Judge = judge.client }
}

type crash struct {
	parent string
	home   string
	cwd    string
}

func crashDuringEval(t *testing.T, code string, ready func(h *harness, parent string)) crash {
	t.Helper()
	hold := make(chan struct{})
	t.Cleanup(func() { close(hold) })
	crashed := newHarness(t, withJudge(newJudgeServer(t, hold)))
	crashed.model.push(evalCall(code))
	parent := crashed.run("run the cell")
	ready(crashed, parent.ID())
	home := t.TempDir()
	copyHome(t, crashed.home, home)
	return crash{parent: parent.ID(), home: home, cwd: crashed.cwd}
}

func (c crash) revive(t *testing.T, judge *judgeServer) []event.Seqd {
	t.Helper()
	revived := newHarness(t, withJudge(judge), func(h *harness) { h.cwd, h.cfg.Config.Cwd = c.cwd, c.cwd })
	revived.model.push(text("cell finished"))
	revived.open(c.home)
	if err := revived.host.Recover(t.Context()); err != nil {
		t.Fatal(err)
	}
	parent, err := revived.host.Resume(t.Context(), c.parent)
	if err != nil {
		t.Fatal(err)
	}
	return revived.idle(parent)
}

func holdCommand() string {
	if runtime.GOOS == "windows" {
		return "ping -n 30 127.0.0.1 >nul"
	}
	return "sleep 30"
}

func nestedDone(callID string) func([]event.Seqd) bool {
	return func(events []event.Seqd) bool {
		_, ok := outputs(events)[callID]
		return ok
	}
}

func TestScriptReplaysRecordedEffectsAfterCrash(t *testing.T) {
	t.Parallel()
	code := "tools.exec(command = \"echo one >> log.txt\")\ntools.exec(command = \"echo two >> log.txt\")\na = ctx.judge(\"hold\", {\"ok\": {\"type\": \"noul\"}})\na[\"ok\"].noul"
	c := crashDuringEval(t, code, func(h *harness, parent string) { h.await(parent, nestedDone("e/2")) })
	judge := newJudgeServer(t, nil)
	events := c.revive(t, judge)
	log, err := os.ReadFile(filepath.Join(c.cwd, "log.txt"))
	if err != nil || !slices.Equal(strings.Fields(string(log)), []string{"one", "two"}) {
		t.Fatalf("log = %q, %v", log, err)
	}
	if got := outputs(events)["e"].Output; !strings.Contains(got, `"status":"completed"`) || !strings.Contains(got, `"value":0.9`) {
		t.Fatalf("eval output = %s", got)
	}
	if judge.requests.Load() != 1 || count(events, event.TypeScriptCompleted) != 1 {
		t.Fatalf("judge requests = %d, script.completed = %d", judge.requests.Load(), count(events, event.TypeScriptCompleted))
	}
}

func TestEvalKilledMidExecRecoversIndeterminate(t *testing.T) {
	t.Parallel()
	c := crashDuringEval(t, "tools.exec(command = \""+holdCommand()+"\")\n1", func(h *harness, parent string) {
		h.await(parent, func(events []event.Seqd) bool { return count(events, event.TypeToolStarted) == 2 })
	})
	events := c.revive(t, newJudgeServer(t, nil))
	done := outputs(events)
	if !strings.Contains(done["e"].Output, `"status":"indeterminate"`) || done["e"].OK {
		t.Fatalf("eval output = %+v", done["e"])
	}
	if done["e/1"].OK || done["e/1"].Recovered != "interrupted" {
		t.Fatalf("nested exec = %+v", done["e/1"])
	}
	for _, item := range events {
		if completed, ok := item.Event.(event.ScriptCompleted); ok && completed.OK {
			t.Fatalf("script.completed = %+v", completed)
		}
	}
}

func TestJudgeBatchReplaysSettledItems(t *testing.T) {
	t.Parallel()
	code := "r = ctx.judge_batch([\"a\", \"b\", \"hold\"], {\"ok\": {\"type\": \"noul\"}}, concurrency = 3)\n[x.answers[\"ok\"].noul for x in r]"
	c := crashDuringEval(t, code, func(h *harness, parent string) {
		h.await(parent, func(events []event.Seqd) bool { return count(events, event.TypeScriptEffect) >= 2 })
	})
	judge := newJudgeServer(t, nil)
	events := c.revive(t, judge)
	if got := outputs(events)["e"].Output; !strings.Contains(got, `"value":[0.9,0.9,0.9]`) {
		t.Fatalf("eval output = %s", got)
	}
	if judge.requests.Load() != 1 {
		t.Fatalf("judge requests after recovery = %d, want 1", judge.requests.Load())
	}
}

type roleProvider struct {
	mu      sync.Mutex
	models  []string
	replies map[string][]reply
}

func (p *roleProvider) respond(w http.ResponseWriter, r *http.Request) {
	var request struct {
		Model string `json:"model"`
	}
	if err := json.UnmarshalRead(r.Body, &request); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	next, ok := p.next(request.Model)
	if !ok {
		http.Error(w, "no reply queued for model "+request.Model, http.StatusNotFound)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = io.WriteString(w, next.body)
}

func (p *roleProvider) next(modelName string) (reply, bool) {
	p.mu.Lock()
	defer p.mu.Unlock()
	p.models = append(p.models, modelName)
	queued := p.replies[modelName]
	if len(queued) == 0 {
		return reply{}, false
	}
	p.replies[modelName] = queued[1:]
	return queued[0], true
}

func (p *roleProvider) seen() []string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return slices.Clone(p.models)
}

func TestJudgeRoleResolvesAgainstTheThreadModel(t *testing.T) {
	provider := &roleProvider{replies: map[string][]reply{
		"gpt-test":   {evalCall("a = ctx.judge(\"state\", {\"ok\": {\"type\": \"noul\"}}, model = \"smol\")\na[\"ok\"].noul"), text("done")},
		"gpt-6-luna": {text(`{"ok":0.9}`)},
	}}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"},{"id":"gpt-6-luna"}]}`)
	})
	mux.HandleFunc("POST /responses", provider.respond)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	t.Setenv("OPENAI_BASE_URL", server.URL)
	t.Setenv("OPENAI_API_KEY", "key")
	logger := slog.New(slog.DiscardHandler)
	cfg := config.Config{
		Home: t.TempDir(), Cwd: t.TempDir(), Approval: config.ApprovalYolo, Sandbox: config.SandboxOff,
		Product: config.ProductDalgona, Reasoning: config.ReasoningDefault, StreamStartTimeout: time.Minute, StreamIdleTimeout: time.Minute,
		Providers: map[string]config.ProviderSpec{"lab": {BaseURL: server.URL + "/lab"}},
	}
	batteries := func(_ context.Context, base app.Base) (app.Batteries, error) {
		return app.Batteries{Extension: scripthost.New(scripthost.Config{Judge: base.Judge, Mode: config.ToolModeEvalOnly, Logger: logger})}, nil
	}
	host, closeHost, err := app.NewHost(t.Context(), app.Options{Product: config.ProductDalgona, Config: &cfg, Logger: logger, Batteries: batteries})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = closeHost() })
	thread, err := host.Start(t.Context(), agent.ThreadSpec{Model: "lab/m"})
	if err != nil {
		t.Fatal(err)
	}
	if err := thread.Submit(t.Context(), agent.Input{Kind: agent.InputPrompt, Text: "judge it", Model: "openai/gpt-test"}); err != nil {
		t.Fatal(err)
	}
	output := outputs(settled(t, thread))["e"].Output
	if !slices.Contains(provider.seen(), "gpt-6-luna") || !strings.Contains(output, `"value":0.9`) {
		t.Fatalf("provider models = %q, eval output = %s", provider.seen(), output)
	}
}

func settled(t *testing.T, thread *agent.Thread) []event.Seqd {
	t.Helper()
	for deadline := time.Now().Add(20 * time.Second); time.Now().Before(deadline); time.Sleep(5 * time.Millisecond) {
		status, err := thread.Status()
		if err != nil {
			t.Fatal(err)
		}
		if !status.Busy {
			records, _, err := thread.Records()
			if err != nil {
				t.Fatal(err)
			}
			return records
		}
	}
	t.Fatalf("thread %s stayed busy", thread.ID())
	return nil
}

const policyPlugin = "def deny(ctx, event):\n    if event['tool'] == 'exec':\n        return {'decision': 'deny', 'reason': 'hooks forbid exec'}\n    return None\non('tool_call', deny)\n"

func withPolicy(t *testing.T) func(*harness) {
	return func(h *harness) { h.scripts.Plugins = loadPlugins(t, map[string]string{"policy.star": policyPlugin}) }
}

func TestPluginHookDeclinesThroughTheGate(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withPolicy(t), func(h *harness) { h.scripts.Mode = config.ToolModeNormal })
	h.model.push(toolCall("x", "exec", `{"command":"echo hi"}`), text("done"))
	events := h.idle(h.run("run it"))
	resolved := decisions(events)["x"]
	if resolved.By != "hook:policy.star:deny" || resolved.Reason != "hooks forbid exec" || count(events, event.TypeToolStarted) != 0 {
		t.Fatalf("resolved = %+v", resolved)
	}
	if count(events, event.TypeScriptStarted) != 1 || count(events, event.TypeScriptCompleted) != 1 {
		t.Fatalf("hook runs: %d started, %d completed", count(events, event.TypeScriptStarted), count(events, event.TypeScriptCompleted))
	}
}

func TestScriptToolDenialStopsTheCell(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withPolicy(t))
	h.model.push(evalCall("tools.exec(command = \"echo hi\")"), text("done"))
	done := outputs(h.idle(h.run("run the cell")))
	if got := done["e"].Output; !strings.Contains(got, `"status":"denied"`) || !strings.Contains(got, "hooks forbid exec") {
		t.Fatalf("eval output = %s", got)
	}
	if nested, ok := done["e/1"]; !ok || nested.OK {
		t.Fatalf("nested call = %+v", nested)
	}
}

func TestDirectCallToFoldedToolIsDeclined(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.model.push(toolCall("x", "exec", `{"command":"echo hi"}`), text("done"))
	done := outputs(h.idle(h.run("run it")))
	if want := "tool exec is not available in tool_mode eval-only; call it from eval"; done["x"].Output != want {
		t.Fatalf("direct call = %+v", done["x"])
	}
}

func TestCellSpawnsChildrenThroughTaskAndSpawn(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) {
		h.scripts.Mode = config.ToolModeNormal
		h.scripts.Eval = config.EvalConfig{Uses: []string{"task"}, Set: true}
	})
	h.model.routes["child a"] = []reply{yield("ya", `"A"`)}
	h.model.routes["child b"] = []reply{text("B done")}
	h.model.push(evalCall("r = tools.task(prompt = \"child a\")\nw = ctx.wait([ctx.spawn(\"child b\")])\n[r, w[0]]"), text("done"))
	events := h.idle(h.run("run the cell"))
	if got := outputs(events)["e"].Output; !strings.Contains(got, `"value":["{\"data\":\"A\"}","B done"]`) {
		t.Fatalf("eval output = %s", got)
	}
	if spawned := count(events, event.TypeChildSpawned); spawned != 2 || h.model.requests("child a") != 1 {
		t.Fatalf("child.spawned = %d, child a requests = %d", spawned, h.model.requests("child a"))
	}
}
