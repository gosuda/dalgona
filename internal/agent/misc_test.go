package agent

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/tool"
)

func (h *harness) waitEvent(id string, match func(event.Event) bool) {
	h.t.Helper()
	for deadline := time.Now().Add(20 * time.Second); time.Now().Before(deadline); time.Sleep(5 * time.Millisecond) {
		for _, item := range h.events(id) {
			if match(item.Event) {
				return
			}
		}
	}
	h.t.Fatalf("event not journaled; events: %v", h.types(id))
}

func TestInterruptDuringBatchSkipsUnstartedCalls(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalYolo))
	h.write("a.txt", "a\n")
	h.model.push(calls(
		call{id: "c1", name: "exec", args: `{"command":"sleep 30"}`},
		call{id: "c2", name: "read", args: `{"path":"a.txt"}`},
		call{id: "c3", name: "read", args: `{"path":"a.txt"}`},
	), text("resumed"))
	thread := h.start()
	h.submit(thread, "batch")
	h.waitEvent(thread.ID(), func(e event.Event) bool {
		started, ok := e.(event.ToolStarted)
		return ok && started.CallID == "c1"
	})
	thread.Interrupt()
	h.waitIdle(thread)
	assertInterruptedBatch(t, h.events(thread.ID()))
	h.submit(thread, "go on")
	h.waitIdle(thread)
	results := callOutputs(h.model.request(1))
	if len(results) != 3 || results["c2"] != "skipped: the turn was interrupted" {
		t.Fatalf("next request results = %v", results)
	}
}

func assertInterruptedBatch(t *testing.T, events []event.Seqd) {
	t.Helper()
	outputs := completedOutputs(events)
	if outputs["c1"].OK || started(events, "c2") || started(events, "c3") {
		t.Fatalf("outputs = %+v", outputs)
	}
	for _, id := range []string{"c2", "c3"} {
		if outputs[id].Recovered != "skipped" || outputs[id].Output != "skipped: the turn was interrupted" {
			t.Fatalf("%s = %+v", id, outputs[id])
		}
	}
	if turn := lastTurn(events); turn.Status != statusInterrupted {
		t.Fatalf("turn = %+v", turn)
	}
}

func TestSecretsAreMaskedInJournalAndUnmaskedInExec(t *testing.T) {
	const secret = "sk-test-123456789"
	t.Setenv("OPENAI_API_KEY", secret)
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		store, err := auth.New(h.home)
		if err != nil {
			t.Fatal(err)
		}
		h.cfg.Auth = store
	})
	h.model.push(
		calls(call{id: "e1", name: "exec", args: `{"command":"env"}`}),
		calls(call{id: "e2", name: "exec", args: `{"command":"echo {{secret:OPENAI_API_KEY}} > k"}`}),
		text("done"),
	)
	thread := h.start()
	h.submit(thread, "print the env; my key is "+secret)
	h.waitIdle(thread)
	if out := completedOutputs(h.events(thread.ID()))["e1"].Output; !strings.Contains(out, "{{secret:OPENAI_API_KEY}}") {
		t.Fatalf("env output = %q", out)
	}
	if strings.Contains(h.journalBytes(thread.ID()), secret) {
		t.Fatal("journal bytes contain the secret")
	}
	for i := range h.model.count() {
		if strings.Contains(h.model.raw(i), secret) {
			t.Fatalf("model request %d contains the secret", i)
		}
	}
	data, err := os.ReadFile(filepath.Join(h.cwd, "k"))
	if err != nil || strings.TrimSpace(string(data)) != secret {
		t.Fatalf("k = %q, %v", data, err)
	}
}

type modelList struct{ ids []string }

func (m modelList) Stream(context.Context, model.Request) (model.Stream, error) {
	return nil, errors.New("modelList: stream is not used")
}

func (m modelList) Models(context.Context) ([]model.Model, error) {
	out := make([]model.Model, len(m.ids))
	for i, id := range m.ids {
		out[i] = model.Model{ID: id}
	}
	return out, nil
}

func withAnthropicModels(ids ...string) func(*harness) {
	return func(h *harness) {
		h.cfg.Providers = func(RetryFunc) (*model.Registry, error) {
			registry := model.NewRegistry()
			return registry, registry.Register("anthropic", modelList{ids: ids})
		}
	}
}

func TestRoleResolutionUsesPresetsAndFallsBack(t *testing.T) {
	t.Parallel()
	const thread = "anthropic/claude-opus-5-5"
	offered := newHarness(t, withAnthropicModels("claude-opus-5-5", "claude-haiku-5-5"))
	if got, err := offered.host.ResolveRole(t.Context(), "smol", thread); err != nil || got != "anthropic/claude-haiku-5-5" {
		t.Fatalf("smol = %q, %v", got, err)
	}
	missing := newHarness(t, withAnthropicModels("claude-opus-5-5"))
	if got, err := missing.host.ResolveRole(t.Context(), "smol", thread); err != nil || got != thread {
		t.Fatalf("fallback = %q, %v", got, err)
	}
	if !strings.Contains(missing.logs.String(), `msg="roles.smol preset anthropic/claude-haiku-5-5 is not offered; using anthropic/claude-opus-5-5"`) {
		t.Fatalf("logs = %s", missing.logs.String())
	}
	configured := newHarness(t, withAnthropicModels(), func(h *harness) { h.cfg.Config.Roles.Smol = "openai/gpt-6-luna" })
	if got, err := configured.host.ResolveRole(t.Context(), "smol", thread); err != nil || got != "openai/gpt-6-luna" {
		t.Fatalf("configured = %q, %v", got, err)
	}
}

type loginLater struct {
	modelList
	calls atomic.Int32
}

func (l *loginLater) Models(ctx context.Context) ([]model.Model, error) {
	if l.calls.Add(1) == 1 {
		return nil, nil
	}
	return l.modelList.Models(ctx)
}

func TestRoleResolutionSeesModelsListedAfterAnEmptyList(t *testing.T) {
	t.Parallel()
	const thread = "anthropic/claude-opus-5-5"
	provider := &loginLater{modelList: modelList{ids: []string{"claude-opus-5-5", "claude-haiku-5-5"}}}
	h := newHarness(t, func(h *harness) {
		h.cfg.Providers = func(RetryFunc) (*model.Registry, error) {
			registry := model.NewRegistry()
			return registry, registry.Register("anthropic", provider)
		}
	})
	if got, err := h.host.ResolveRole(t.Context(), "smol", thread); err != nil || got != thread {
		t.Fatalf("smol before login = %q, %v", got, err)
	}
	if got, err := h.host.ResolveRole(t.Context(), "smol", thread); err != nil || got != "anthropic/claude-haiku-5-5" {
		t.Fatalf("smol after login = %q, %v", got, err)
	}
}

func turnIDs(events []event.Seqd) []string {
	var ids []string
	for _, item := range events {
		if started, ok := item.Event.(event.TurnStarted); ok {
			ids = append(ids, started.TurnID)
		}
	}
	return ids
}

func (h *harness) twoTurns() (*Thread, []string) {
	h.t.Helper()
	thread := h.start()
	h.submit(thread, "first prompt")
	h.waitIdle(thread)
	h.submit(thread, "second prompt")
	h.waitIdle(thread)
	return thread, turnIDs(h.events(thread.ID()))
}

func (h *harness) promptTexts(thread *Thread, prompt string) []string {
	h.t.Helper()
	h.submit(thread, prompt)
	h.waitIdle(thread)
	return userTexts(h.model.request(h.model.count() - 1))
}

func TestRevertAndLeafMoveTheActivePath(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread, turns := h.twoTurns()
	if err := thread.Revert("missing"); !errors.Is(err, ErrTurnNotOnPath) || err.Error() != "turn missing is not on the active path" {
		t.Fatalf("revert missing = %v", err)
	}
	if err := thread.SetLeaf("missing"); !errors.Is(err, ErrUnknownTurn) {
		t.Fatalf("leaf missing = %v", err)
	}
	if err := thread.Revert(turns[1]); err != nil {
		t.Fatal(err)
	}
	if texts := h.promptTexts(thread, "third prompt"); strings.Join(texts, "|") != "first prompt|third prompt" {
		t.Fatalf("after revert texts = %q", texts)
	}
	if err := thread.SetLeaf(turns[1]); err != nil {
		t.Fatal(err)
	}
	if texts := h.promptTexts(thread, "fourth prompt"); strings.Join(texts, "|") != "first prompt|second prompt|fourth prompt" {
		t.Fatalf("after leaf move texts = %q", texts)
	}
}

func TestForkCopiesActivePathPrefix(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread, turns := h.twoTurns()
	child, err := thread.Fork(t.Context(), turns[0])
	if err != nil {
		t.Fatal(err)
	}
	info, ok := forkInfo(h.host.Threads(), child.ID())
	copied := turnIDs(h.events(child.ID()))
	if !ok || info.ForkOf != thread.ID() || strings.Join(copied, "|") != turns[0] {
		t.Fatalf("fork events = %v, info = %+v", h.types(child.ID()), info)
	}
	if texts := h.promptTexts(child, "forked prompt"); strings.Join(texts, "|") != "first prompt|forked prompt" {
		t.Fatalf("fork texts = %q", texts)
	}
}

func forkInfo(threads []ThreadInfo, id string) (ThreadInfo, bool) {
	for _, info := range threads {
		if info.ID == id {
			return info, true
		}
	}
	return ThreadInfo{}, false
}

func TestBranchingRefusesRunningThread(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	gate := make(chan struct{})
	h.model.push(response{body: sseText("slow"), gate: gate})
	thread := h.start()
	h.submit(thread, "slow")
	h.waitRequests(1)
	err := thread.Revert("x")
	if !errors.Is(err, ErrThreadRunning) || err.Error() != "thread "+thread.ID()+" is running" {
		t.Fatalf("revert while running = %v", err)
	}
	if _, err := thread.Fork(t.Context(), ""); !errors.Is(err, ErrThreadRunning) {
		t.Fatalf("fork while running = %v", err)
	}
	if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "again"}); !errors.Is(err, ErrThreadRunning) {
		t.Fatalf("prompt while running = %v", err)
	}
	close(gate)
	h.waitIdle(thread)
}

type rendezvousAgent struct{ arrived *sync.WaitGroup }

func (rendezvousAgent) Spec() tool.Spec {
	return tool.Spec{Name: "fanout", Description: "subagent", Schema: jsontext.Value(`{"type":"object"}`)}
}
func (rendezvousAgent) Class() tool.Class { return tool.ClassAgent }
func (r rendezvousAgent) Run(ctx context.Context, call tool.Call, _ tool.Env) (tool.Result, error) {
	r.arrived.Done()
	together := make(chan struct{})
	go func() { r.arrived.Wait(); close(together) }()
	select {
	case <-together:
		return tool.Result{Output: []byte("child " + call.ID), OK: true}, nil
	case <-time.After(5 * time.Second):
		return tool.Result{Output: []byte("alone " + call.ID)}, nil
	case <-ctx.Done():
		return tool.Result{}, ctx.Err()
	}
}

func TestConsecutiveAgentCallsRunTogether(t *testing.T) {
	t.Parallel()
	var arrived sync.WaitGroup
	arrived.Add(2)
	h := newHarness(t, func(h *harness) {
		h.cfg.Tools = func(string, string) ([]tool.Tool, error) { return []tool.Tool{rendezvousAgent{arrived: &arrived}}, nil }
	})
	h.model.push(calls(call{id: "k1", name: "fanout", args: `{}`}, call{id: "k2", name: "fanout", args: `{}`}), text("done"))
	h.idleThread("spawn two")
	results := callOutputs(h.model.request(1))
	if results["k1"] != "child k1" || results["k2"] != "child k2" {
		t.Fatalf("agent calls did not run together: %v", results)
	}
}

type denyExec struct{ baseExtension }

func (denyExec) ToolCall(_ context.Context, call tool.Call) (HookVerdict, error) {
	if call.Name == "exec" {
		return HookVerdict{Decision: HookDeny, Reason: "hooks forbid exec", By: "hook:policy.star:deny"}, nil
	}
	return HookVerdict{}, nil
}

func TestHookVerdictComesFirst(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Extension = func(*Thread) Extension { return denyExec{} }
	})
	events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"echo hi"}`})
	resolved := resolutions(events)["x"]
	if resolved.By != "hook:policy.star:deny" || resolved.Reason != "hooks forbid exec" || started(events, "x") {
		t.Fatalf("resolved = %+v", resolved)
	}
}
