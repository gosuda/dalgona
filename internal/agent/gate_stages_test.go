package agent

import (
	"context"
	"slices"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
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
