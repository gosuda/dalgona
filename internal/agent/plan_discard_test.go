package agent

import (
	"context"
	"encoding/json/jsontext"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
)

type discardFakeTool struct {
	discards *atomic.Int64
	applied  *atomic.Int64
}

func (d *discardFakeTool) Spec() tool.Spec {
	return tool.Spec{Name: "discardfake", Description: "fake editor", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (d *discardFakeTool) Class() tool.Class { return tool.ClassEdit }

func (d *discardFakeTool) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	return tool.Result{Output: []byte("run"), OK: true}, nil
}

func (d *discardFakeTool) Plan(context.Context, tool.Call, tool.Env) (tool.Plan, error) {
	return tool.Plan{
		Apply: func(context.Context) (tool.Result, error) {
			d.applied.Add(1)
			return tool.Result{Output: []byte("ok"), OK: true}, nil
		},
		Discard: func() { d.discards.Add(1) },
	}, nil
}

func TestDeclinedPlanIsDiscarded(t *testing.T) {
	t.Parallel()
	var discards, applied atomic.Int64
	h := newHarness(t, withApproval(config.ApprovalAsk), func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{&discardFakeTool{discards: &discards, applied: &applied}}, nil
		}
	})
	events := runCalls(t, h, call{id: "d1", name: "discardfake", args: `{}`})
	if got := discards.Load(); got != 1 {
		t.Fatalf("discards = %d, want 1", got)
	}
	if got := applied.Load(); got != 0 {
		t.Fatalf("applied = %d, want 0", got)
	}
	if resolved := resolutions(events)["d1"]; resolved.Decision != "decline" {
		t.Fatalf("resolution = %+v, want decline", resolved)
	}
	if started(events, "d1") {
		t.Fatal("declined call journaled tool.started")
	}
}

func TestAcceptedPlanIsNotDiscarded(t *testing.T) {
	t.Parallel()
	var discards, applied atomic.Int64
	h := newHarness(t, withApproval(config.ApprovalAuto), func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{&discardFakeTool{discards: &discards, applied: &applied}}, nil
		}
	})
	events := runCalls(t, h, call{id: "a1", name: "discardfake", args: `{}`})
	if got := applied.Load(); got != 1 {
		t.Fatalf("applied = %d, want 1", got)
	}
	if got := discards.Load(); got != 0 {
		t.Fatalf("discards = %d, want 0", got)
	}
	if !started(events, "a1") {
		t.Fatal("accepted call did not journal tool.started")
	}
	if completed := completedOutputs(events)["a1"]; !completed.OK {
		t.Fatalf("completed = %+v, want OK", completed)
	}
}

type failApplyTool struct {
	discards *atomic.Int64
}

func (f *failApplyTool) Spec() tool.Spec {
	return tool.Spec{Name: "failapply", Description: "fails apply", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (f *failApplyTool) Class() tool.Class { return tool.ClassEdit }

func (f *failApplyTool) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	return tool.Result{Output: []byte("run"), OK: true}, nil
}

func (f *failApplyTool) Plan(context.Context, tool.Call, tool.Env) (tool.Plan, error) {
	return tool.Plan{
		Apply: func(context.Context) (tool.Result, error) {
			return tool.Result{}, context.DeadlineExceeded
		},
		Discard: func() { f.discards.Add(1) },
	}, nil
}

func TestPlanDiscardOnApplyFailure(t *testing.T) {
	t.Parallel()
	var discards atomic.Int64
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{&failApplyTool{discards: &discards}}, nil
		}
	})
	events := runCalls(t, h, call{id: "f1", name: "failapply", args: `{}`})
	if got := discards.Load(); got != 1 {
		t.Fatalf("discards = %d, want 1 after failed apply", got)
	}
	if completedOutputs(events)["f1"].OK {
		t.Fatal("failed apply reported OK")
	}
}

func TestDiscardPlanIsIdempotent(t *testing.T) {
	t.Parallel()
	var discards atomic.Int64
	p := &prepared{plan: &tool.Plan{Discard: func() { discards.Add(1) }}}
	p.discardPlan()
	p.discardPlan()
	if got := discards.Load(); got != 1 {
		t.Fatalf("discards = %d, want 1 after two discardPlan calls", got)
	}
	if p.plan != nil {
		t.Fatal("plan not cleared after discardPlan")
	}
}

func TestAdmitGateErrorDiscardsPlan(t *testing.T) {
	t.Parallel()
	var discards atomic.Int64
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{&discardFakeTool{discards: &discards, applied: &atomic.Int64{}}}, nil
		}
	})
	thread := h.start()
	ctx := t.Context()
	if _, err := thread.appendEvents(ctx, event.TurnStarted{TurnID: "turn-test"}); err != nil {
		t.Fatal(err)
	}
	w := &worker{t: thread, turnID: "turn-test"}
	if err := w.prime(ctx); err != nil {
		t.Fatal(err)
	}
	item, ok := w.tools.byName["discardfake"]
	if !ok {
		t.Fatal("discardfake tool missing")
	}
	plan, err := item.(tool.Editor).Plan(ctx, tool.Call{ID: "g1", Name: "discardfake"}, w.env(verdict{}))
	if err != nil {
		t.Fatal(err)
	}
	p := &prepared{call: event.ToolCall{ID: "g1", Name: "discardfake", Args: jsontext.Value(`{}`)}, tool: item, class: item.Class(), args: jsontext.Value(`{}`), plan: &plan}
	cancelled, cancel := context.WithCancel(ctx)
	cancel()
	if _, err := w.admit(cancelled, p, "req-g1"); err == nil {
		t.Fatal("admit with cancelled context succeeded")
	}
	if got := discards.Load(); got != 1 {
		t.Fatalf("discards = %d, want 1 after gate error", got)
	}
}

func TestAdmitStartFailureDiscardsPlan(t *testing.T) {
	t.Parallel()
	var discards atomic.Int64
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{&discardFakeTool{discards: &discards, applied: &atomic.Int64{}}}, nil
		}
	})
	thread := h.start()
	ctx := t.Context()
	if _, err := thread.appendEvents(ctx, event.TurnStarted{TurnID: "turn-test"}); err != nil {
		t.Fatal(err)
	}
	w := &worker{t: thread, turnID: "turn-test"}
	if err := w.prime(ctx); err != nil {
		t.Fatal(err)
	}
	item := w.tools.byName["discardfake"]
	plan, err := item.(tool.Editor).Plan(ctx, tool.Call{ID: "s1", Name: "discardfake"}, w.env(verdict{}))
	if err != nil {
		t.Fatal(err)
	}
	plan.Files = []tool.FileChange{{Path: "", PreSHA: "bad"}}
	p := &prepared{call: event.ToolCall{ID: "s1", Name: "discardfake", Args: jsontext.Value(`{}`)}, tool: item, class: item.Class(), args: jsontext.Value(`{}`), plan: &plan}
	if _, err := w.admit(ctx, p, "req-s1"); err == nil {
		t.Fatal("admit with invalid plan files succeeded")
	}
	if got := discards.Load(); got != 1 {
		t.Fatalf("discards = %d, want 1 after start failure", got)
	}
}

type batchDiscardAgent struct {
	discards *atomic.Int64
	applied  *atomic.Int64
}

func (b *batchDiscardAgent) Spec() tool.Spec {
	return tool.Spec{Name: "batchdiscard", Description: "batch fake editor", Schema: jsontext.Value(`{"type":"object"}`)}
}

func (b *batchDiscardAgent) Class() tool.Class { return tool.ClassAgent }

func (b *batchDiscardAgent) Run(context.Context, tool.Call, tool.Env) (tool.Result, error) {
	return tool.Result{Output: []byte("run"), OK: true}, nil
}

func (b *batchDiscardAgent) Plan(context.Context, tool.Call, tool.Env) (tool.Plan, error) {
	return tool.Plan{
		Apply: func(context.Context) (tool.Result, error) {
			b.applied.Add(1)
			return tool.Result{Output: []byte("applied"), OK: true}, nil
		},
		Discard: func() { b.discards.Add(1) },
	}, nil
}

func TestAgentBatchPrepareFailureDiscardsReadyPlans(t *testing.T) {
	t.Parallel()
	var discards, applied atomic.Int64
	h := newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Tools = func(context.Context, string, string) ([]tool.Tool, error) {
			return []tool.Tool{&batchDiscardAgent{discards: &discards, applied: &applied}}, nil
		}
	})
	thread := h.start()
	ctx := t.Context()
	if _, err := thread.appendEvents(ctx, event.TurnStarted{TurnID: "turn-test"}); err != nil {
		t.Fatal(err)
	}
	w := &worker{t: thread, turnID: "turn-test"}
	if err := w.prime(ctx); err != nil {
		t.Fatal(err)
	}
	calls := []event.ToolCall{
		{ID: "b1", Name: "batchdiscard", Args: jsontext.Value(`{}`)},
		{ID: "b2", Name: "batchdiscard", Args: jsontext.Value(`{}`), ArgsBlob: &event.BlobRef{Blob: "missing", Bytes: 2}},
	}
	if err := w.runAgentBatch(ctx, calls); err == nil {
		t.Fatal("runAgentBatch with failing second prepare succeeded")
	}
	if got := discards.Load(); got != 1 {
		t.Fatalf("discards = %d, want 1 after batch prepare failure", got)
	}
	if got := applied.Load(); got != 0 {
		t.Fatalf("applied = %d, want 0 after batch prepare failure", got)
	}
}
