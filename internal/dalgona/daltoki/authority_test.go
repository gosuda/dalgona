package daltoki

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/graph"
)

func unknownProfileRaw() config.DaltokiConfig {
	return config.DaltokiConfig{
		Enabled: true, AppID: 7, PrivateKeyFile: "/keys/app.pem",
		Repositories:        []string{"octo/repo"},
		PollIntervalSeconds: 60, Concurrency: 2, JobsPerDay: 20,
		Model: "model", Profile: "no-such-style",
		JobTokenLimit: 100000, DailyTokenLimit: 500000,
		JobCostLimitUSDMicros: 5000000, DailyCostLimitUSDMicros: 20000000,
	}
}

func TestParseConfigAcceptsKnownProfile(t *testing.T) {
	t.Parallel()
	raw := unknownProfileRaw()
	raw.Profile = "hashline-light"
	cfg, err := ParseConfig(raw, okUnmask)
	if err != nil {
		t.Fatalf("ParseConfig known profile: %v", err)
	}
	if cfg.Profile != "hashline-light" {
		t.Fatalf("profile = %q, want hashline-light", cfg.Profile)
	}
}

func TestNodeThreadSpecSetsReadOnlyAndProfile(t *testing.T) {
	t.Parallel()
	plan := nodeThreadSpec(NodeRun{Node: graph.Node{ID: "plan"}})
	if !plan.ReadOnly || plan.Profile != "" {
		t.Fatalf("plan spec = %+v", plan)
	}
	fix := nodeThreadSpec(NodeRun{
		Node:      graph.Node{ID: "fix", Profile: "hashline-light", Writes: []string{"."}},
		Workspace: "/work", Model: "model",
	})
	if fix.ReadOnly || fix.Profile != "hashline-light" {
		t.Fatalf("fix spec = %+v", fix)
	}
	if fix.Cwd != "/work" || fix.Model != "model" {
		t.Fatalf("fix routing = %+v", fix)
	}
}
