package advisor

import (
	"context"
	"encoding/json/jsontext"
	"log/slog"
	"path/filepath"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

func hookedHarness(t *testing.T, configure func(*config.Config)) (*harness, *Registry) {
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
	registry := NewRegistry()
	t.Cleanup(func() { _ = registry.Close() })
	store, err := journal.Open(h.home)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = store.Close() })
	cfg := h.cfg
	cfg.Home = h.home
	var host *agent.Host
	host, err = agent.NewHost(agent.HostConfig{
		Journal: store, Config: cfg, Providers: h.providers, Tools: h.tools,
		Runner: func(_ context.Context, id, _ string) (runner.Runner, error) {
			return runner.NewLocal(filepath.Join(h.home, "sessions", id, "runs"))
		},
		Logger: slog.New(slog.DiscardHandler),
		OnChildStarted: func(child *agent.Thread, req agent.TaskRequest) {
			attach := registry.Attach
			if req.Advisor {
				attach = registry.AttachChild
			}
			if _, attachErr := attach(context.Background(), host, child); attachErr != nil {
				t.Error(attachErr)
			}
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = host.Close() })
	h.store, h.host = store, host
	return h, registry
}

func spawnChild(t *testing.T, h *harness, callID string, req agent.TaskRequest) string {
	t.Helper()
	parent := startThread(t, h, agent.ThreadSpec{})
	seedTaskCall(t, parent, callID)
	child, err := parent.StartChild(t.Context(), callID, req)
	if err != nil {
		t.Fatal(err)
	}
	return child
}

func seedTaskCall(t *testing.T, parent *agent.Thread, callID string) {
	t.Helper()
	started := event.ToolStarted{CallID: callID, TurnID: "turn-seed", Tool: "task", Args: jsontext.Value(`{}`), Class: "agent"}
	if err := parent.Append(t.Context(), event.TurnStarted{TurnID: "turn-seed"}, started); err != nil {
		t.Fatal(err)
	}
}

func holdAdvisorReview(t *testing.T, h *harness, first response) func() {
	t.Helper()
	gate := make(chan struct{})
	release := sync.OnceFunc(func() { close(gate) })
	t.Cleanup(release)
	h.model.push(roleAdvisor, gated(first, gate), text("done"))
	return release
}

func TestSpawnedChildWithGlobalOptInGetsReview(t *testing.T) {
	t.Parallel()
	h, registry := hookedHarness(t, func(cfg *config.Config) {
		cfg.Advisor.Subagents = true
	})
	release := holdAdvisorReview(t, h, calls(advise("n1", "check the error path", "nit")))
	child := spawnChild(t, h, "call-1", agent.TaskRequest{Prompt: "do the work"})
	rt := registry.Get(child)
	if rt == nil {
		t.Fatal("advisor runtime is missing for a globally opted-in subagent child")
	}
	h.waitFor("the default advisor review to start", func() bool {
		return advisorStates(t, rt)[config.DefaultAdvisorName] == StateReviewing
	})
	requireAdvisorStates(t, rt, map[string]State{config.DefaultAdvisorName: StateReviewing})
	release()
	h.waitFor("the subagent review note", func() bool {
		return len(notesIn(journalEvents(t, h.store, child))) == 1
	})
}

func TestSpawnedChildWithEntryOptInGetsReview(t *testing.T) {
	t.Parallel()
	h, registry := hookedHarness(t, func(cfg *config.Config) {
		cfg.Advisor.Subagents = false
		cfg.Advisors = subagentsRoster()
	})
	release := holdAdvisorReview(t, h, calls(advise("n1", "check the error path", "nit")))
	child := spawnChild(t, h, "call-1", agent.TaskRequest{Prompt: "do the work"})
	rt := registry.Get(child)
	if rt == nil {
		t.Fatal("advisor runtime is missing for an entry opted-in subagent child")
	}
	h.waitFor("the force advisor review to start", func() bool {
		return advisorStates(t, rt)["force"] == StateReviewing
	})
	requireAdvisorStates(t, rt, map[string]State{
		"force": StateReviewing, "off": StateDisabled, "inherit": StateDisabled,
	})
	release()
	h.waitFor("the entry review note", func() bool {
		return len(notesIn(journalEvents(t, h.store, child))) == 1
	})
	notes := notesIn(journalEvents(t, h.store, child))
	if notes[0].Advisor != "force" {
		t.Fatalf("note advisor = %q, want force", notes[0].Advisor)
	}
}

func TestSpawnedChildWithoutOptInGetsNoAdvisor(t *testing.T) {
	t.Parallel()
	h, registry := hookedHarness(t, nil)
	child := spawnChild(t, h, "call-1", agent.TaskRequest{Prompt: "do the work"})
	if rt := registry.Get(child); rt != nil {
		t.Fatalf("advisor runtime %+v attached without any subagent opt-in", rt)
	}
	if _, ok := h.host.AdvisorChild(child, config.DefaultAdvisorName); ok {
		t.Fatal("advisor session started without any subagent opt-in")
	}
	thread, err := h.host.Resume(t.Context(), child)
	if err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	if notes := notesIn(journalEvents(t, h.store, child)); len(notes) != 0 {
		t.Fatalf("notes = %+v, want none without any subagent opt-in", notes)
	}
}
