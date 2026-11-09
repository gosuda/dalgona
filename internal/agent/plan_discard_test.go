package agent

import (
	"context"
	"encoding/json/jsontext"
	"sync/atomic"
	"testing"

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
		h.cfg.Tools = func(string, string) ([]tool.Tool, error) {
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
		h.cfg.Tools = func(string, string) ([]tool.Tool, error) {
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
