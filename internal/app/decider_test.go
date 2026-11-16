package app

import (
	json "encoding/json/v2"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/decide"
)

func TestDecideWithoutHostDeclinesThroughFallback(t *testing.T) {
	j := &judges{
		roles: &roleResolver{},
		spec:  decide.Spec{Adapter: decide.AdapterLLM, Model: "smol"},
	}
	got, err := j.decider().Decide(t.Context(), agent.Action{Thread: "thread-x"})
	if err != nil {
		t.Fatalf("Decide without a host = %v, want a decline", err)
	}
	if got.Allow {
		t.Fatalf("decision = %+v, want a decline", got)
	}
	if want := "declined by decider smol: judge model smol: decider: host is not ready"; got.Reason != want {
		t.Fatalf("reason = %q, want %q", got.Reason, want)
	}
}

type judgeRecorder struct {
	mu    sync.Mutex
	auth  string
	model string
	score float64
}

func (j *judgeRecorder) setScore(score float64) {
	j.mu.Lock()
	j.score = score
	j.mu.Unlock()
}

func (j *judgeRecorder) lastModel() string {
	j.mu.Lock()
	defer j.mu.Unlock()
	return j.model
}

func (j *judgeRecorder) lastAuth() string {
	j.mu.Lock()
	defer j.mu.Unlock()
	return j.auth
}

func (j *judgeRecorder) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	var req struct {
		Model string `json:"model"`
	}
	if err := json.Unmarshal(body, &req); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	j.mu.Lock()
	j.auth = r.Header.Get("Authorization")
	j.model = req.Model
	score := j.score
	j.mu.Unlock()

	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = fmt.Fprintf(w, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"{\\\"reversible\\\":%v}\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n", score)
}

func TestDeciderRoutesConfiguredJudgeOnReadyHost(t *testing.T) {
	recorder := &judgeRecorder{score: 0.95}
	server := httptest.NewServer(recorder)
	t.Cleanup(server.Close)

	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	t.Setenv("LOCAL_KEY", "secret-key")

	cfg := config.Config{
		Home:    home,
		Product: config.ProductDalgona,
		Roles: config.RolesConfig{
			Smol: "local/non-judge-model",
		},
		ApprovalDecider: config.ApprovalDeciderConfig{
			Threshold: 0.90,
			Decider:   "llm:local/judge-model",
		},
		Providers: map[string]config.ProviderSpec{
			"local": {
				BaseURL:   server.URL,
				APIKeyEnv: "LOCAL_KEY",
			},
		},
	}

	host, closeHost, err := NewHost(t.Context(), Options{
		Product: config.ProductDalgona,
		Cwd:     t.TempDir(),
		Config:  &cfg,
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := closeHost(); closeErr != nil {
			t.Errorf("close host: %v", closeErr)
		}
	})

	thread, err := host.Start(t.Context(), agent.ThreadSpec{
		Cwd:   t.TempDir(),
		Model: "local/working-model",
	})
	if err != nil {
		t.Fatal(err)
	}

	roles := &roleResolver{}
	roles.set(host)
	store, err := auth.NewWithPatterns(cfg.Home, nil)
	if err != nil {
		t.Fatal(err)
	}
	creds := newCredentials(store, cfg.Product, nil)
	j, err := newJudges(t.Context(), cfg, creds, roles)
	if err != nil {
		t.Fatal(err)
	}

	decider := j.decider()

	action := agent.Action{
		Thread:   thread.ID(),
		Tool:     "exec",
		Commands: []string{"git status"},
		Project:  t.TempDir(),
	}

	for _, tc := range []judgeDecisionExpectation{
		{score: 0.95, allow: true},
		{score: 0.20, allow: false},
	} {
		recorder.setScore(tc.score)
		got, decideErr := decider.Decide(t.Context(), action)
		if decideErr != nil {
			t.Fatalf("Decide with score %v: %v", tc.score, decideErr)
		}
		assertConfiguredJudgeDecision(t, recorder, got, tc)
	}
}

type judgeDecisionExpectation struct {
	score float64
	allow bool
}

func assertConfiguredJudgeDecision(t *testing.T, recorder *judgeRecorder, got agent.Decision, want judgeDecisionExpectation) {
	t.Helper()
	if got.Allow != want.allow {
		t.Fatalf("decision.Allow = %v, want %v for score %v (reason: %s)", got.Allow, want.allow, want.score, got.Reason)
	}
	if got.By != "decider:local/judge-model" {
		t.Fatalf("decision.By = %q, want decider:local/judge-model", got.By)
	}
	if got.Scores["reversible"] != want.score {
		t.Fatalf("decision.Scores[reversible] = %v, want %v", got.Scores["reversible"], want.score)
	}
	if model := recorder.lastModel(); model != "judge-model" {
		t.Fatalf("wire model = %q, want judge-model", model)
	}
	if auth := recorder.lastAuth(); auth != "Bearer secret-key" {
		t.Fatalf("wire authorization = %q, want Bearer secret-key", auth)
	}
}

func unavailableJudgeServer(t *testing.T) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Retry-After", "1")
		http.Error(w, "judge primary unavailable", http.StatusServiceUnavailable)
	}))
	t.Cleanup(server.Close)
	return server
}

func fallbackJudges(t *testing.T, decider, smol string) (*judges, string) {
	t.Helper()
	good := &judgeRecorder{score: 0.95}
	goodServer := httptest.NewServer(good)
	t.Cleanup(goodServer.Close)
	brokenURL := unavailableJudgeServer(t).URL
	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	clearProviderKeys(t)
	t.Setenv("BROKEN_KEY", "broken-secret")
	cfg := config.Config{
		Home:    home,
		Product: config.ProductDalgona,
		Roles:   config.RolesConfig{Smol: smol},
		ApprovalDecider: config.ApprovalDeciderConfig{
			Threshold: 0.90,
			Decider:   decider,
		},
		Providers: map[string]config.ProviderSpec{
			"broken": {BaseURL: brokenURL, APIKeyEnv: "BROKEN_KEY"},
			"local":  {BaseURL: goodServer.URL, APIKeyEnv: "LOCAL_KEY"},
		},
	}
	t.Setenv("LOCAL_KEY", "local-secret")
	host, closeHost, err := NewHost(t.Context(), Options{Product: config.ProductDalgona, Cwd: t.TempDir(), Config: &cfg})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if closeErr := closeHost(); closeErr != nil {
			t.Errorf("close host: %v", closeErr)
		}
	})
	thread, err := host.Start(t.Context(), agent.ThreadSpec{
		Cwd:   t.TempDir(),
		Model: "local/working-model",
	})
	if err != nil {
		t.Fatal(err)
	}
	roles := &roleResolver{}
	roles.set(host)
	store, err := auth.NewWithPatterns(cfg.Home, nil)
	if err != nil {
		t.Fatal(err)
	}
	j, err := newJudges(t.Context(), cfg, newCredentials(store, cfg.Product, nil), roles)
	if err != nil {
		t.Fatal(err)
	}
	return j, thread.ID()
}

type judgeFallbackCase struct {
	name     string
	decider  string
	smol     string
	allow    bool
	by       string
	contains []string
}

func TestJudgeFallbackCatchesConstructionFailures(t *testing.T) {
	for _, tc := range []judgeFallbackCase{
		{
			name: "unknown provider falls back to smol", decider: "llm:nope/judge-primary", smol: "local/qwen-small",
			allow: true, by: "decider:local/qwen-small",
			contains: []string{"fallback from llm:nope/judge-primary: judge model nope/judge-primary: unknown provider nope"},
		},
		{
			name: "missing credential falls back to smol", decider: "decisions:typesafe/jev-1.14", smol: "local/qwen-small",
			allow: true, by: "decider:local/qwen-small",
			contains: []string{"fallback from decisions:typesafe/jev-1.14", "openrouter"},
		},
		{
			name: "primary http failure falls back to smol", decider: "llm:broken/judge-primary", smol: "local/qwen-small",
			allow: true, by: "decider:local/qwen-small",
			contains: []string{"fallback from llm:broken/judge-primary", "503"},
		},
		{
			name: "smol construction failure declines with both causes", decider: "llm:broken/judge-primary", smol: "missing/qwen-small",
			allow: false, by: "decider:broken/judge-primary",
			contains: []string{"503", "unknown provider missing", "fallback llm:missing/qwen-small"},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			j, threadID := fallbackJudges(t, tc.decider, tc.smol)
			assertFallbackDecision(t, j, threadID, tc)
		})
	}
}

func assertFallbackDecision(t *testing.T, j *judges, threadID string, tc judgeFallbackCase) {
	t.Helper()
	action := agent.Action{Tool: "exec", Commands: []string{"git status"}, Project: t.TempDir()}
	got, err := j.decider().Decide(t.Context(), agent.Action{Thread: threadID, Tool: action.Tool, Commands: action.Commands, Project: action.Project})
	if err != nil {
		t.Fatalf("Decide error = %v", err)
	}
	if got.Allow != tc.allow {
		t.Fatalf("Allow = %v, want %v (reason %s)", got.Allow, tc.allow, got.Reason)
	}
	if got.By != tc.by {
		t.Fatalf("By = %q, want %q", got.By, tc.by)
	}
	for _, want := range tc.contains {
		if !strings.Contains(got.Reason, want) {
			t.Fatalf("reason %q misses %q", got.Reason, want)
		}
	}
}
