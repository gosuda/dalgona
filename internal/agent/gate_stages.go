package agent

import (
	"cmp"
	"context"

	"github.com/gosuda/dalgona/internal/agent/rules"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/tool"
)

type gateRun struct {
	gc        gateCall
	requestID string
	approval  config.Approval
	hook      verdict
}

func (w *worker) decide(ctx context.Context, gc gateCall, requestID string) verdict {
	run := w.gatePrepare(gc, requestID)
	if v, ok := gateMandatory(run); ok {
		return v
	}
	if v, ok := w.gateHooks(ctx, &run); ok {
		return v
	}
	if v, ok := w.gateSandbox(ctx, run); ok {
		return v
	}
	if v, ok := gateApproval(run); ok {
		return v
	}
	return w.gateDecider(ctx, run)
}

func (w *worker) gatePrepare(gc gateCall, requestID string) gateRun {
	return gateRun{gc: gc, requestID: requestID, approval: w.settings.Approval}
}

func (w *worker) gateHooks(ctx context.Context, run *gateRun) (verdict, bool) {
	call := tool.Call{ID: run.gc.call.ID, Name: run.gc.call.Name, Args: run.gc.args}
	result, err := w.t.ext.ToolCall(withWorker(ctx, w), call)
	if err != nil {
		w.t.host.log.Warn("agent: tool_call hook", "thread", w.t.id, "tool", run.gc.call.Name, "error", err)
	}
	switch result.Decision {
	case HookAllow:
		run.hook = accept(result.By)
	case HookDeny:
		return decline(result.By, cmp.Or(result.Reason, "declined by "+result.By)), true
	}
	return verdict{}, false
}

func gateApproval(run gateRun) (verdict, bool) {
	if run.approval == config.ApprovalYolo {
		v := accept("mode:yolo")
		v.unsandboxed = true
		return v, true
	}
	if v, ok := mechanical(run.gc, run.approval); ok {
		return v, true
	}
	return run.hook, run.hook.allow
}

func mechanical(gc gateCall, approval config.Approval) (verdict, bool) {
	by := "mode:" + string(approval)
	switch gc.class {
	case tool.ClassRead, tool.ClassAgent, tool.ClassScript:
		return accept(by), true
	case tool.ClassEdit:
		return accept(by), approval == config.ApprovalAuto && len(gc.outside) == 0
	case tool.ClassExec, tool.ClassRemote:
		return verdict{}, false
	}
	return verdict{}, false
}

func gateMandatory(run gateRun) (verdict, bool) {
	if run.gc.class == tool.ClassRemote {
		return staticRemoteRule(run.gc)
	}
	if run.gc.class != tool.ClassExec {
		return verdict{}, false
	}
	name, part, matched := rules.Check(run.gc.command)
	if !matched {
		return verdict{}, false
	}
	return decline("rule:"+name, "declined by rule "+name+": "+part), true
}

func (w *worker) gateDecider(ctx context.Context, run gateRun) verdict {
	if run.approval == config.ApprovalAsk {
		return w.human(ctx, run.gc, run.requestID)
	}
	return w.automated(ctx, run.gc)
}
