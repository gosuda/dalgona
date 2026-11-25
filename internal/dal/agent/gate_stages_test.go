package agent

import (
	"context"
	"slices"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

type stageHook struct {
	baseExtension
	decision string
	calls    *atomic.Int64
}

func (h stageHook) ToolCall(_ context.Context, call tool.Call) (HookVerdict, error) {
	if call.Name != "exec" {
		return HookVerdict{}, nil
	}
	if h.calls != nil {
		h.calls.Add(1)
	}
	return HookVerdict{Decision: h.decision, Reason: "hook " + h.decision, By: "hook:order.star:" + h.decision}, nil
}

func withStageHook(decision string) func(*harness) {
	return func(h *harness) {
		h.cfg.Extension = func(*Thread) Extension { return stageHook{decision: decision} }
	}
}

func withAvailableSandbox(approval config.Approval) func(*harness) {
	return func(h *harness) {
		h.cfg.Config.Approval = approval
		h.cfg.Config.Sandbox = config.SandboxWorkspaceWrite
		h.cfg.SandboxCause = func() string { return "" }
	}
}

func TestGateStagesRunInOrder(t *testing.T) {
	t.Parallel()
	const flagged = "rm -rf gate-stage-order-absent"
	const plain = "echo hi"
	cases := []struct {
		name      string
		configure []func(*harness)
		command   string
		by        string
		started   bool
		decider   int64
	}{
		{
			name:      "mandatory alone declines a flagged command",
			configure: []func(*harness){withApproval(config.ApprovalAuto)},
			command:   flagged,
			by:        "rule:recursive-delete",
		},
		{
			name:      "mandatory precedes hook denial",
			configure: []func(*harness){withApproval(config.ApprovalAuto), withStageHook(HookDeny)},
			command:   flagged,
			by:        "rule:recursive-delete",
		},
		{
			name:      "mandatory precedes hook allowance",
			configure: []func(*harness){withApproval(config.ApprovalAuto), withStageHook(HookAllow)},
			command:   flagged,
			by:        "rule:recursive-delete",
		},
		{
			name:      "mandatory precedes yolo",
			configure: []func(*harness){withApproval(config.ApprovalYolo)},
			command:   flagged,
			by:        "rule:recursive-delete",
		},
		{
			name:      "sandbox alone declines a plain command without a client",
			configure: []func(*harness){withUnavailableSandbox(config.ApprovalAsk)},
			command:   plain,
			by:        "sandbox",
		},
		{
			name:      "mandatory runs before sandbox",
			configure: []func(*harness){withUnavailableSandbox(config.ApprovalAsk)},
			command:   flagged,
			by:        "rule:recursive-delete",
		},
		{
			name:      "sandbox precedes hook allowance",
			configure: []func(*harness){withUnavailableSandbox(config.ApprovalAsk), withStageHook(HookAllow)},
			command:   plain,
			by:        "sandbox",
		},
		{
			name:      "sandbox precedes yolo",
			configure: []func(*harness){withUnavailableSandbox(config.ApprovalYolo)},
			command:   plain,
			by:        "sandbox",
		},
		{
			name:      "decider alone decides when the sandbox is available",
			configure: []func(*harness){withAvailableSandbox(config.ApprovalAuto)},
			command:   plain,
			by:        "decider:count",
			started:   true,
			decider:   1,
		},
		{
			name:      "sandbox runs before decider",
			configure: []func(*harness){withUnavailableSandbox(config.ApprovalAuto)},
			command:   plain,
			by:        "sandbox",
		},
		{
			name:      "hook denial precedes sandbox",
			configure: []func(*harness){withUnavailableSandbox(config.ApprovalAuto), withStageHook(HookDeny)},
			command:   plain,
			by:        "hook:order.star:deny",
		},
		{
			name:      "hook allowance skips decider after sandbox",
			configure: []func(*harness){withAvailableSandbox(config.ApprovalAuto), withStageHook(HookAllow)},
			command:   plain,
			by:        "hook:order.star:allow",
			started:   true,
		},
		{
			name:      "yolo accepts after mandatory and sandbox",
			configure: []func(*harness){withAvailableSandbox(config.ApprovalYolo)},
			command:   plain,
			by:        "mode:yolo",
			started:   true,
		},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			counter := &countingDecider{}
			h := newHarness(t, slices.Concat(test.configure, []func(*harness){func(h *harness) { h.cfg.Decider = counter }})...)
			events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"` + test.command + `"}`})
			resolved := resolutions(events)["x"]
			if resolved.By != test.by || started(events, "x") != test.started || counter.calls.Load() != test.decider {
				t.Fatalf("by = %q, started = %v, decider calls = %d, want %q, %v, %d (resolved %+v)",
					resolved.By, started(events, "x"), counter.calls.Load(), test.by, test.started, test.decider, resolved)
			}
		})
	}
}

func TestMandatoryDeclineDoesNotRunHooks(t *testing.T) {
	t.Parallel()
	for _, command := range []string{"rm -rf gate-stage-order-absent", "echo hi"} {
		t.Run(command, func(t *testing.T) {
			t.Parallel()
			var calls atomic.Int64
			h := newHarness(t, withAvailableSandbox(config.ApprovalAuto), func(h *harness) {
				h.cfg.Extension = func(*Thread) Extension {
					return stageHook{decision: HookAllow, calls: &calls}
				}
			})
			events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"` + command + `"}`})
			want := int64(0)
			if command == "echo hi" {
				want = 1
			}
			if calls.Load() != want || started(events, "x") != (want == 1) {
				t.Fatalf("hook calls = %d, started = %v, want %d, %v", calls.Load(), started(events, "x"), want, want == 1)
			}
		})
	}
}

type denyingDecider struct{ calls atomic.Int64 }

func (c *denyingDecider) Decide(context.Context, Action) (Decision, error) {
	c.calls.Add(1)
	return Decision{Allow: false, By: "decider:test", Reason: "declined by decider:test"}, nil
}

func approvalRequests(events []event.Seqd) map[string]string {
	requests := make(map[string]string)
	for _, item := range events {
		if value, ok := item.Event.(event.ApprovalRequested); ok {
			requests[value.RequestID] = value.CallID
		}
	}
	return requests
}

func requestIDFor(events []event.Seqd, callID string) string {
	for id, call := range approvalRequests(events) {
		if call == callID {
			return id
		}
	}
	return ""
}

func countApprovals(events []event.Seqd, callID string) (int, int) {
	want := requestIDFor(events, callID)
	if want == "" {
		return 0, 0
	}
	return countResolutions(events, want)
}

func countResolutions(events []event.Seqd, want string) (requested, resolved int) {
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.ApprovalRequested:
			if value.RequestID == want {
				requested++
			}
		case event.ApprovalResolved:
			if value.RequestID == want {
				resolved++
			}
		}
	}
	return requested, resolved
}

func TestGateHookAllowCannotBypassMandatoryDeny(t *testing.T) {
	t.Parallel()
	decider := &denyingDecider{}
	h := newHarness(t, slices.Concat([]func(*harness){
		withAvailableSandbox(config.ApprovalAuto),
		withStageHook(HookAllow),
		func(h *harness) { h.cfg.Decider = decider },
	})...)
	events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"rm -rf gate-stage-order-absent"}`})
	resolved := resolutions(events)["x"]
	requested, resolvedCount := countApprovals(events, "x")
	if resolved.By != "rule:recursive-delete" || decider.calls.Load() != 0 || started(events, "x") {
		t.Fatalf("by = %q, decider calls = %d, started = %v, want rule deny with no decider and no start", resolved.By, decider.calls.Load(), started(events, "x"))
	}
	if requested != 1 || resolvedCount != 1 {
		t.Fatalf("requested = %d, resolved = %d, want exactly one of each", requested, resolvedCount)
	}
}

func TestGateDeciderDeclineIsFinal(t *testing.T) {
	t.Parallel()
	decider := &denyingDecider{}
	h := newHarness(t, withAvailableSandbox(config.ApprovalAuto), func(h *harness) { h.cfg.Decider = decider })
	events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"echo hi"}`})
	resolved := resolutions(events)["x"]
	requested, resolvedCount := countApprovals(events, "x")
	if resolved.Decision != "decline" || resolved.By != "decider:test" || started(events, "x") {
		t.Fatalf("resolved = %+v, started = %v, want final decider decline with no start", resolved, started(events, "x"))
	}
	if decider.calls.Load() != 1 || requested != 1 || resolvedCount != 1 {
		t.Fatalf("decider calls = %d, requested = %d, resolved = %d, want 1, 1, 1", decider.calls.Load(), requested, resolvedCount)
	}
}

func TestGateHumanDecisionIsJournaledOnce(t *testing.T) {
	t.Parallel()
	h := newHarness(t, withAvailableSandbox(config.ApprovalAsk))
	h.model.push(calls(call{id: "x", name: "exec", args: `{"command":"echo hi"}`}), text("done"))
	thread := h.start()
	client := answerApprovals(t, thread, func(ApprovalPrompt) Reply { return ReplyAccept })
	h.submit(thread, "run the task")
	h.waitIdle(thread)
	client.stop()
	events := h.events(thread.ID())
	resolved := resolutions(events)["x"]
	requested, resolvedCount := countApprovals(events, "x")
	if resolved.Decision != "accept" || resolved.By != "user" || !started(events, "x") {
		t.Fatalf("resolved = %+v, started = %v, want user accept with start", resolved, started(events, "x"))
	}
	if requested != 1 || resolvedCount != 1 {
		t.Fatalf("requested = %d, resolved = %d, want exactly one of each", requested, resolvedCount)
	}
}

func TestGateCommitJournalsDeciderVerdictOnce(t *testing.T) {
	t.Parallel()
	decider := &denyingDecider{}
	h := newHarness(t, withAvailableSandbox(config.ApprovalAuto), func(h *harness) { h.cfg.Decider = decider })
	events := runCalls(t, h, call{id: "x", name: "exec", args: `{"command":"echo hi"}`})
	requested, resolvedCount := countApprovals(events, "x")
	if requested != 1 || resolvedCount != 1 {
		t.Fatalf("requested = %d, resolved = %d, want exactly one of each from Commit", requested, resolvedCount)
	}
	requestedSeq, resolvedSeq := resolutionOrder(events)
	if requestedSeq == 0 || resolvedSeq == 0 || requestedSeq >= resolvedSeq {
		t.Fatalf("requested seq = %d, resolved seq = %d, want requested before resolved", requestedSeq, resolvedSeq)
	}
	if decider.calls.Load() != 1 || started(events, "x") {
		t.Fatalf("decider calls = %d, started = %v, want one decider call and no start", decider.calls.Load(), started(events, "x"))
	}
}

func resolutionOrder(events []event.Seqd) (requestedSeq, resolvedSeq uint64) {
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.ApprovalRequested:
			if value.CallID == "x" {
				requestedSeq = item.Seq
			}
		case event.ApprovalResolved:
			if resolved, ok := resolutions(events)["x"]; ok && value.By == resolved.By {
				resolvedSeq = item.Seq
			}
		}
	}
	return requestedSeq, resolvedSeq
}
