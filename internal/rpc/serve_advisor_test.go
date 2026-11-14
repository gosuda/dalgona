package rpc

import (
	"context"
	"log/slog"
	"path/filepath"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/advisor"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/read"
)

func newAdvisorRig(t *testing.T, registry *advisor.Registry) *rig {
	t.Helper()
	r := &rig{t: t, home: t.TempDir(), cwd: t.TempDir(), llm: newFakeLLM(t)}
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
	host, err := agent.NewHost(agent.HostConfig{
		Journal: store, Auth: credentials, ProviderIDs: []string{"openai"}, Config: advisorConfig(r),
		Providers: r.providers,
		Tools:     func(context.Context, string, string) ([]tool.Tool, error) { return []tool.Tool{read.New()}, nil },
		Runner: func(_ context.Context, id, _ string) (runner.Runner, error) {
			return runner.NewLocal(filepath.Join(r.home, "sessions", id, "runs"))
		},
		Logger: slog.New(slog.DiscardHandler),
		OnChildStarted: func(child *agent.Thread, req agent.TaskRequest) {
			attach := registry.Attach
			if req.Advisor {
				attach = registry.AttachChild
			}
			if _, attachErr := attach(context.Background(), r.host, child); attachErr != nil {
				t.Error(attachErr)
			}
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	r.host = host
	t.Cleanup(func() { _ = host.Close() })
	t.Cleanup(func() { _ = registry.Close() })
	return r
}

func advisorConfig(r *rig) config.Config {
	return config.Config{
		Home: r.home, Cwd: r.cwd, Model: "openai/gpt-test", Approval: config.ApprovalAuto, Sandbox: config.SandboxOff,
		Product: config.ProductDalgona, Reasoning: config.ReasoningDefault,
		Advisor: config.AdvisorConfig{
			Enabled: true, Review: config.AdvisorReviewTurn, Interval: 1, Sync: config.AdvisorSyncOff,
			ContinueSeverity: "blocker", ImmuneTurns: 3, MaxNotes: 3,
		},
		Advisors: map[string]config.AdvisorSpec{config.DefaultAdvisorName: {
			Review: config.AdvisorReviewTurn, Interval: 1, Sync: config.AdvisorSyncOff, Enabled: true,
		}},
	}
}

func childThread(t *testing.T, host *agent.Host, parent string) string {
	t.Helper()
	deadline := time.Now().Add(waitLimit)
	for time.Now().Before(deadline) {
		for _, info := range host.AllThreads() {
			if info.Parent == parent && info.Advisor == "" {
				return info.ID
			}
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatalf("no task child of %s", parent)
	return ""
}

func advisorStates(c *wireClient, threadID string) []AdvisorState {
	var states []AdvisorState
	c.ok("dalgona/advisor/status", map[string]any{"threadId": threadID}, &states)
	return states
}

func settledAdvisor(t *testing.T, c *wireClient, threadID string) []AdvisorState {
	t.Helper()
	deadline := time.Now().Add(waitLimit)
	for {
		states := advisorStates(c, threadID)
		if len(states) != 1 || states[0].Status != string(advisor.StateReviewing) || time.Now().After(deadline) {
			return states
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func TestTaskChildAdvisorIsServedFromTheHostRegistry(t *testing.T) {
	registry := advisor.NewRegistry()
	r := newAdvisorRig(t, registry)
	c := r.connectWith(ServeOptions{Advisors: registry})
	r.llm.push(
		llmReply{body: sseCall("task-1", "task", `{"prompt":"check the tree","advisor":true}`)},
		llmReply{body: sseCall("yield-1", "yield", `{"data":"ok"}`)},
	)
	parent := startThread(t, r, c, nil)
	expect(t, registry.Get(parent) != nil, "registry runtime for the user thread", parent)
	startTurn(t, c, parent, "delegate")
	c.await("turn/completed", func(m message) bool { return fieldOf(t, m.Params, "threadId") == parent })
	child := childThread(t, r.host, parent)
	attached := registry.Get(child)
	expect(t, attached != nil, "registry runtime for the task child", child)

	states := settledAdvisor(t, c, child)
	expect(t, len(states) == 1 && states[0].Status == string(advisor.StateIdle), "child advisor status", states)
	c.ok("dalgona/advisor/set", map[string]any{"threadId": child, "enabled": false}, nil)
	states = advisorStates(c, child)
	expect(t, len(states) == 1 && states[0].Status == string(advisor.StateDisabled), "status after disable", states)
	held, err := attached.Status()
	expect(t, err == nil && len(held) == 1 && held[0].State == advisor.StateDisabled, "registry runtime after rpc disable", held)
	expect(t, registry.Get(child) == attached, "registry runtime identity", registry.Get(child))

	expect(t, registry.Close() == nil, "registry close", child)
	expect(t, registry.Get(child) == nil, "runtime after close", registry.Get(child))
	c.fails("dalgona/advisor/status", map[string]any{"threadId": child}, codeInvalidRequest, advisor.ErrRegistryClosed.Error())
	expect(t, registry.Close() == nil, "second registry close", child)
}
