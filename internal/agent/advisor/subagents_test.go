package advisor

import (
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
)

func subagentsRoster() map[string]config.AdvisorSpec {
	base := config.AdvisorSpec{
		Review: config.AdvisorReviewTurn, Interval: 1, Sync: config.AdvisorSyncOff,
		Tools: []string{"read", "search"}, Enabled: true,
	}
	force := base
	force.Subagents = config.AdvisorSubagentsOn
	off := base
	off.Subagents = config.AdvisorSubagentsOff
	return map[string]config.AdvisorSpec{"force": force, "off": off, "inherit": base}
}

func advisorStates(t *testing.T, rt *Runtime) map[string]State {
	t.Helper()
	states, err := rt.Status()
	if err != nil {
		t.Fatal(err)
	}
	out := make(map[string]State, len(states))
	for _, s := range states {
		out[s.Name] = s.State
	}
	return out
}

func requireAdvisorStates(t *testing.T, rt *Runtime, want map[string]State) {
	t.Helper()
	got := advisorStates(t, rt)
	if len(got) != len(want) {
		t.Fatalf("states = %v, want %v", got, want)
	}
	for name, state := range want {
		if got[name] != state {
			t.Fatalf("states = %v, want %v", got, want)
		}
	}
}

func TestAttach_perAdvisorSubagentsOverride(t *testing.T) {
	t.Parallel()
	setup := func(t *testing.T, global bool) *harness {
		t.Helper()
		return newHarness(t, func(cfg *config.Config) {
			cfg.Advisor.Subagents = global
			cfg.Advisors = subagentsRoster()
		})
	}
	startChild := func(t *testing.T, h *harness) *agent.Thread {
		t.Helper()
		parent := startThread(t, h, agent.ThreadSpec{})
		return startThread(t, h, agent.ThreadSpec{Parent: parent.ID()})
	}

	t.Run("global off without opt-in", func(t *testing.T) {
		t.Parallel()
		h := setup(t, false)
		requireAdvisorStates(t, h.attach(h.host, startChild(t, h)), map[string]State{
			"force": StateIdle, "off": StateDisabled, "inherit": StateDisabled,
		})
	})

	t.Run("global on", func(t *testing.T) {
		t.Parallel()
		h := setup(t, true)
		requireAdvisorStates(t, h.attach(h.host, startChild(t, h)), map[string]State{
			"force": StateIdle, "off": StateDisabled, "inherit": StateIdle,
		})
	})

	t.Run("task opt-in", func(t *testing.T) {
		t.Parallel()
		h := setup(t, false)
		registry := NewRegistry()
		t.Cleanup(func() { _ = registry.Close() })
		rt, err := registry.AttachChild(t.Context(), h.host, startChild(t, h))
		if err != nil {
			t.Fatal(err)
		}
		requireAdvisorStates(t, rt, map[string]State{
			"force": StateIdle, "off": StateDisabled, "inherit": StateIdle,
		})
	})

	t.Run("none apply stays disabled", func(t *testing.T) {
		t.Parallel()
		h := newHarness(t, func(cfg *config.Config) {
			cfg.Advisor.Subagents = false
			roster := subagentsRoster()
			delete(roster, "force")
			cfg.Advisors = roster
		})
		requireAdvisorStates(t, h.attach(h.host, startChild(t, h)), map[string]State{
			"off": StateDisabled, "inherit": StateDisabled,
		})
	})

	t.Run("top level unaffected", func(t *testing.T) {
		t.Parallel()
		h := setup(t, false)
		requireAdvisorStates(t, h.attach(h.host, startThread(t, h, agent.ThreadSpec{})), map[string]State{
			"force": StateIdle, "off": StateIdle, "inherit": StateIdle,
		})
	})
}
