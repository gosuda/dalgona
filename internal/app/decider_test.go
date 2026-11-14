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

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/decide"
)

func TestDecideWithoutHostReportsNotReady(t *testing.T) {
	j := &judges{
		roles: &roleResolver{},
		spec:  decide.Spec{Adapter: decide.AdapterLLM, Model: "smol"},
	}
	_, err := j.decider().Decide(t.Context(), agent.Action{Thread: "thread-x"})
	if err == nil || !strings.Contains(err.Error(), "host is not ready") {
		t.Fatalf("Decide without a host = %v, want host-not-ready", err)
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
