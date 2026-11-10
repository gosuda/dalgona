package agent

import (
	"context"
	"errors"
	"slices"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/prompt"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/task"
)

func reasoningEffort(request map[string]any) any {
	reasoning, _ := request["reasoning"].(map[string]any)
	return reasoning["effort"]
}

func withPresetModel(h *harness) {
	h.cfg.Config.Model = "openai/gpt-6.1-sol"
	h.model.models = []string{"gpt-6.1-sol"}
}

func TestTurnEffortOverridesConfigAndUsesPresetEfforts(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withPresetModel, func(h *harness) { h.cfg.Config.Reasoning = config.ReasoningLow })
	thread := h.start()
	h.submit(thread, "configured effort")
	h.waitIdle(thread)
	if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "turn effort", Effort: "xhigh"}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	h.submit(thread, "ultrathink with a turn effort")
	h.waitIdle(thread)
	got := []any{reasoningEffort(h.model.request(0)), reasoningEffort(h.model.request(1)), reasoningEffort(h.model.request(2))}
	if got[0] != "low" || got[1] != "xhigh" || got[2] != "max" {
		t.Fatalf("efforts = %v", got)
	}
	if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "bad", Effort: "loud"}); err == nil {
		t.Fatal("an unknown effort was accepted")
	}
}

func TestTurnEffortSurvivesRecovery(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withPresetModel)
	h.seed(
		event.TurnStarted{TurnID: "t"},
		event.UserMessage{ItemID: "u", TurnID: "t", Text: "resume", Kind: "prompt", Effort: "high"},
	)
	h.recoverSession()
	if got := reasoningEffort(h.model.request(0)); got != "high" {
		t.Fatalf("recovered effort = %v", got)
	}
}

func TestRecordRunnerBoundOnceAndForkCarriesIt(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.idleThread("hello")
	bound := event.RunnerBound{Runner: "box", Service: "docker", BoxID: "box-1", Baseline: "abc123"}
	for range 2 {
		if err := h.host.RecordRunnerBound(t.Context(), thread.ID(), bound); err != nil {
			t.Fatal(err)
		}
	}
	if err := h.host.RecordRunnerBound(t.Context(), "missing", bound); err == nil {
		t.Fatal("binding a closed thread succeeded")
	}
	if count := countType(h.events(thread.ID()), event.TypeRunnerBound); count != 1 {
		t.Fatalf("runner.bound count = %d", count)
	}
	child, err := thread.Fork(t.Context(), "")
	if err != nil {
		t.Fatal(err)
	}
	carried, ok := h.events(child.ID())[1].Event.(event.RunnerBound)
	if !ok || carried != bound {
		t.Fatalf("fork binding = %#v", h.events(child.ID())[1].Event)
	}
}

func TestSteerDuringToolRidesTheNextRequest(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withApproval(config.ApprovalYolo))
	h.model.push(calls(call{id: "c1", name: "exec", args: `{"command":"` + holdCommand(1) + `"}`}), text("done with beta"))
	thread := h.start()
	h.submit(thread, "run it")
	h.waitEvent(thread.ID(), func(e event.Event) bool {
		started, ok := e.(event.ToolStarted)
		return ok && started.CallID == "c1"
	})
	if err := thread.Submit(t.Context(), Input{Kind: InputSteer, Text: "also mention beta"}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	if h.model.count() != 2 {
		t.Fatalf("requests = %d, want 2", h.model.count())
	}
	items := inputItems(h.model.request(1))
	outputAt := itemIndex(items, func(item map[string]any) bool { return item["type"] == "function_call_output" })
	steerAt := itemIndex(items, func(item map[string]any) bool { return strings.Contains(textOf(item), "also mention beta") })
	if outputAt < 0 || steerAt != outputAt+1 {
		t.Fatalf("tool output at %d, steer at %d", outputAt, steerAt)
	}
}

type recordingRunner struct {
	runner.Runner
	mu    *sync.Mutex
	modes *[]runner.SandboxMode
}

func (r recordingRunner) Start(ctx context.Context, spec runner.ProcessSpec) error {
	r.mu.Lock()
	*r.modes = append(*r.modes, spec.Sandbox.Mode)
	r.mu.Unlock()
	return r.Runner.Start(ctx, spec)
}

func withRecordingRunner(mu *sync.Mutex, modes *[]runner.SandboxMode) func(*harness) {
	return func(h *harness) {
		h.cfg.Config.Sandbox = config.SandboxWorkspaceWrite
		h.cfg.Runner = func(ctx context.Context, id, name string) (runner.Runner, error) {
			inner, err := h.runner(ctx, id, name)
			return recordingRunner{Runner: inner, mu: mu, modes: modes}, err
		}
	}
}

func TestYoloRunsExecUnsandboxedAndAutoKeepsTheSandbox(t *testing.T) {
	t.Parallel()
	cases := []struct {
		approval config.Approval
		want     runner.SandboxMode
	}{
		{approval: config.ApprovalYolo, want: runner.SandboxOff},
		{approval: config.ApprovalAuto, want: runner.SandboxWorkspaceWrite},
	}
	for _, test := range cases {
		var mu sync.Mutex
		var modes []runner.SandboxMode
		h := newHarness(t, withApproval(test.approval), withRecordingRunner(&mu, &modes), func(h *harness) {
			h.cfg.Decider = &countingDecider{}
		})
		runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"true"}`})
		mu.Lock()
		got := slices.Clone(modes)
		mu.Unlock()
		if len(got) != 1 || got[0] != test.want {
			t.Fatalf("%s: sandbox modes = %v", test.approval, got)
		}
	}
}

func archiveAfterRunning(t *testing.T, h *harness) *Thread {
	t.Helper()
	gate := make(chan struct{})
	h.model.push(response{body: sseText("slow"), gate: gate})
	thread := h.start()
	h.submit(thread, "busy")
	h.waitRequests(1)
	if err := h.host.Archive(t.Context(), thread.ID()); !errors.Is(err, ErrThreadRunning) {
		t.Fatalf("archiving a running thread = %v", err)
	}
	close(gate)
	h.waitIdle(thread)
	if err := h.host.Archive(t.Context(), thread.ID()); err != nil {
		t.Fatal(err)
	}
	return thread
}

func TestArchiveSurvivesReopenAndRefusesInput(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	id := archiveAfterRunning(t, h).ID()
	if err := errors.Join(h.host.Close(), h.journal.Close()); err != nil {
		t.Fatal(err)
	}
	host := h.reopen(h.home)
	if !host.Archived(id) || len(host.Threads()) != 0 || len(host.AllThreads()) != 1 {
		t.Fatalf("archived=%v threads=%d all=%d", host.Archived(id), len(host.Threads()), len(host.AllThreads()))
	}
	reopened, err := host.Resume(t.Context(), id)
	if err != nil {
		t.Fatal(err)
	}
	more := Input{Kind: InputPrompt, Text: "more"}
	if err := reopened.Submit(t.Context(), more); err == nil || err.Error() != "thread "+id+" is archived" {
		t.Fatalf("submit to an archived thread = %v", err)
	}
	if err := errors.Join(host.Unarchive(t.Context(), id), reopened.Submit(t.Context(), more)); err != nil {
		t.Fatal(err)
	}
}

func TestVerbatimInputSkipsNoticesAndMentions(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.write("a.txt", "alpha\n")
	thread := h.start()
	if err := thread.Submit(t.Context(), Input{Kind: InputPrompt, Text: "ultrathink about @a.txt", Verbatim: true}); err != nil {
		t.Fatal(err)
	}
	h.waitIdle(thread)
	for _, item := range h.events(thread.ID()) {
		if message, ok := item.Event.(event.UserMessage); ok && (!message.Verbatim || len(message.Notices)+len(message.Mentions) > 0) {
			t.Fatalf("user.message = %+v", message)
		}
	}
	if got := reasoningEffort(h.model.request(0)); got == "max" {
		t.Fatal("a verbatim keyword raised the effort")
	}
}

func TestToolsAndSkillsFollowTheThreadCwd(t *testing.T) {
	t.Parallel()
	var mu sync.Mutex
	var toolCwds, skillCwds []string
	h := newHarness(t, func(h *harness) {
		h.cfg.Tools = func(ctx context.Context, modelID, cwd string) ([]tool.Tool, error) {
			mu.Lock()
			defer mu.Unlock()
			toolCwds = append(toolCwds, cwd)
			return h.tools(ctx, modelID, cwd)
		}
		h.cfg.Skills = func(cwd string) ([]prompt.Skill, error) {
			mu.Lock()
			defer mu.Unlock()
			skillCwds = append(skillCwds, cwd)
			return nil, nil
		}
	})
	other := t.TempDir()
	thread, err := h.host.Start(t.Context(), ThreadSpec{Cwd: other})
	if err != nil {
		t.Fatal(err)
	}
	h.submit(thread, "hi")
	h.waitIdle(thread)
	mu.Lock()
	defer mu.Unlock()
	if !slices.Equal(toolCwds, []string{other}) || !slices.Equal(skillCwds, []string{other}) {
		t.Fatalf("tool cwds = %v, skill cwds = %v", toolCwds, skillCwds)
	}
}

func TestTaskAdvisorOptInCallsTheChildHook(t *testing.T) {
	t.Parallel()
	var mu sync.Mutex
	var attached []string
	h := newHarness(t, func(h *harness) {
		h.cfg.OnChildStarted = func(child *Thread, _ task.Request) {
			mu.Lock()
			defer mu.Unlock()
			attached = append(attached, child.created.Parent)
		}
	})
	h.model.route = newChildScript(map[string][]response{"child a": {yieldData("ya", "1")}, "child b": {yieldData("yb", "2")}}).route
	h.model.push(calls(
		call{id: "ta", name: "task", args: `{"prompt":"child a","advisor":true}`},
		call{id: "tb", name: "task", args: `{"prompt":"child b"}`},
	), text("done"))
	parent := h.idleThread("spawn")
	mu.Lock()
	defer mu.Unlock()
	if len(attached) != 1 || attached[0] != parent.ID() {
		t.Fatalf("advisor attached for %v", attached)
	}
}
