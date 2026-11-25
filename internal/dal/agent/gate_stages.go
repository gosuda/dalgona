package agent

import (
	"cmp"
	"context"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/agent/rules"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

type gateRun struct {
	gc        gateCall
	requestID string
	approval  config.Approval
	hook      verdict
}

func (w *worker) decide(ctx context.Context, gc gateCall, requestID string) verdict {
	run := w.gatePrepare(gc, requestID)
	if refusal, refused := w.readOnlyRefusal(run.gc.class, run.gc.call.Name); refused {
		return refusal
	}
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
		return w.gateHuman(ctx, run.gc, run.requestID)
	}
	return w.gateAutomated(ctx, run.gc)
}

func (w *worker) gateHuman(ctx context.Context, gc gateCall, requestID string) verdict {
	if w.t.created.Advisor != "" {
		return w.gateAutomated(ctx, gc)
	}
	if prefix := w.matchingGrant(gc); prefix != "" {
		return accept("grant:" + prefix)
	}
	return w.gateAskUser(ctx, gc, requestID)
}

func (w *worker) gateHumanSkippingGrants(ctx context.Context, gc gateCall, requestID string) verdict {
	if w.t.created.Advisor != "" {
		return w.gateAutomated(ctx, gc)
	}
	return w.gateAskUser(ctx, gc, requestID)
}

func (w *worker) gateAutomated(ctx context.Context, gc gateCall) verdict {
	decider := w.t.host.cfg.Decider
	if decider == nil {
		return decline("decider", "declined by decider: no decider is configured")
	}
	decision, err := decider.Decide(ctx, w.action(gc))
	if err != nil {
		return decline("decider", "declined by decider: "+err.Error())
	}
	by := cmp.Or(decision.By, "decider")
	if !decision.Allow {
		v := decline(by, cmp.Or(decision.Reason, "declined by decider"))
		v.scores = decision.Scores
		return v
	}
	v := accept(by)
	v.reason, v.scores, v.unsandboxed = cmp.Or(decision.Reason, v.reason), decision.Scores, gc.escalate
	return v
}

func (w *worker) gateAskUser(ctx context.Context, gc gateCall, requestID string) verdict {
	replies := make(chan Reply, 1)
	prompt, ok := w.approvalPrompt(gc, requestID)
	if !ok {
		return decline("mode:ask", "declined: secret masking failed")
	}
	offered, err := ask(ctx, w.t, func(a *actor) bool { return a.offerApproval(prompt, replies) })
	forwarded, linked := w.t.forwardApproval(prompt, replies)
	if err != nil {
		w.t.discardApproval(requestID)
		if forwarded {
			prompt.Resolved = true
			prompt.Resolution = ApprovalResolution("cancel", interruptedReason)
			w.t.forwardApproval(prompt, nil)
		}
		if ctx.Err() != nil {
			return verdict{decision: "cancel", by: "user", reason: interruptedReason}
		}
		return decline("mode:ask", declineNoClient)
	}
	v := decline("mode:ask", declineNoClient)
	if forwarded || (offered && !linked) {
		v = awaitReply(ctx, replies)
	}
	if forwarded {
		prompt.Resolved = true
		prompt.Resolution = ApprovalResolution(v.decision, v.reason)
		w.t.forwardApproval(prompt, nil)
		w.t.discardApproval(prompt.RequestID)
	}
	return v
}

func (w *worker) gateCommit(ctx context.Context, requestID string, v verdict) (verdict, error) {
	resolved := event.ApprovalResolved{RequestID: requestID, Decision: v.decision, By: v.by, Reason: v.reason, Scores: v.scores}
	_, err := w.t.appendEvents(ctx, resolved)
	return v, err
}
