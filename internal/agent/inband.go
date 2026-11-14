package agent

import (
	"cmp"
	"context"
	"maps"
	"slices"
	"sync"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/rules"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/cursor"
	"github.com/gosuda/dalgona/internal/tool"
)

type inBandCalls struct {
	mu     sync.Mutex
	calls  map[string]event.ToolCall
	events map[string][]event.Event
	cancel bool
}

func newInBandCalls() *inBandCalls {
	return &inBandCalls{calls: make(map[string]event.ToolCall), events: make(map[string][]event.Event)}
}

func (c *inBandCalls) put(call event.ToolCall, events []event.Event) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.calls[call.ID] = call
	c.events[call.ID] = events
}

func (c *inBandCalls) take(callID string) ([]event.Event, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	events, ok := c.events[callID]
	delete(c.calls, callID)
	delete(c.events, callID)
	return events, ok
}

func (c *inBandCalls) drain() ([]event.ToolCall, []event.Event) {
	c.mu.Lock()
	defer c.mu.Unlock()
	ids := slices.Sorted(maps.Keys(c.events))
	calls := make([]event.ToolCall, 0, len(ids))
	var events []event.Event
	for _, id := range ids {
		if call, ok := c.calls[id]; ok {
			calls = append(calls, call)
		}
		events = append(events, c.events[id]...)
	}
	c.calls = make(map[string]event.ToolCall)
	c.events = make(map[string][]event.Event)
	return calls, events
}

func (c *inBandCalls) markCancel() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.cancel = true
}

func (c *inBandCalls) takeCancel() bool {
	if c == nil {
		return false
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	cancelled := c.cancel
	c.cancel = false
	return cancelled
}

func (c *inBandCalls) resolved(callID string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	_, ok := c.calls[callID]
	return ok
}

func (w *worker) interruptTurn() {
	_, _ = ask(w.t, func(a *actor) bool { a.interrupt(); return true })
}

func (w *worker) openStream(ctx context.Context, providerID string, req model.Request) (model.Stream, error) {
	if w.inBand == nil {
		w.inBand = newInBandCalls()
	}
	provider, ok := w.registry.Provider(providerID)
	if native, inBand := provider.(*cursor.Provider); ok && inBand {
		return native.StreamWith(ctx, req, cursor.StreamOptions{InBand: w.runInBand})
	}
	return w.registry.Stream(ctx, providerID, req)
}

func (w *worker) runInBand(ctx context.Context, call model.ToolCall) (model.ToolResult, error) {
	completed, err := w.inBandEvents(ctx, call)
	if err != nil {
		return model.ToolResult{}, err
	}
	return model.ToolResult{ID: call.ID, Name: call.Name, Output: quoteJSON(completed.Output), IsError: !completed.OK}, nil
}

type inBandDenial struct {
	class   tool.Class
	text    string
	verdict verdict
}

func (w *worker) inBandEvents(ctx context.Context, call model.ToolCall) (event.ToolCompleted, error) {
	if err := ctx.Err(); err != nil {
		return event.ToolCompleted{}, err
	}
	args, err := eventArgs(call)
	if err != nil {
		return event.ToolCompleted{}, err
	}
	if w.inBand.takeCancel() {
		w.inBand.markCancel()
		toolCall := event.ToolCall{ID: call.ID, Name: call.Name, Args: args, Freeform: call.Freeform}
		class := tool.ClassRemote
		if w.tools != nil {
			if item, ok := w.tools.byName[call.Name]; ok {
				class = item.Class()
			}
		}
		return w.denyInBand(ctx, toolCall, newID("req-"), inBandDenial{class: class, verdict: verdict{decision: "cancel", by: "user", reason: "cancelled by user", cancel: true}})
	}

	toolCall := event.ToolCall{ID: call.ID, Name: call.Name, Args: args, Freeform: call.Freeform}
	requestID := newID("req-")
	guard, err := w.prepareGuard(ctx, toolCall, requestID)
	if err != nil {
		return event.ToolCompleted{}, err
	}
	if guard.denial != nil {
		return w.denyInBand(ctx, toolCall, requestID, *guard.denial)
	}
	v, err := w.approveInBand(ctx, guard.p, guard.gc, requestID)
	if err != nil {
		guard.p.discardPlan()
		return event.ToolCompleted{}, err
	}
	return w.resolveInBand(ctx, guard.p, toolCall, v)
}

func (w *worker) resolveInBand(ctx context.Context, p *prepared, toolCall event.ToolCall, v verdict) (event.ToolCompleted, error) {
	if v.cancel {
		w.inBand.markCancel()
	}
	if !v.allow {
		p.discardPlan()
		return w.completeInBandDeny(ctx, toolCall, v)
	}
	p.verdict = v
	if err := w.start(ctx, p); err != nil {
		p.discardPlan()
		return event.ToolCompleted{}, err
	}
	return w.executeInBand(ctx, p)
}

func (w *worker) journalCover(ctx context.Context, toolCall event.ToolCall) error {
	status, err := w.t.Status()
	if err != nil {
		return err
	}
	cover := event.AssistantMessage{ItemID: newID("item-"), TurnID: w.turnID, ToolCalls: []event.ToolCall{toolCall}, StopReason: "tool_calls", ContextSeq: uint64(status.Head)}
	_, err = w.t.appendEvents(ctx, cover)
	return err
}

func (w *worker) approveInBand(ctx context.Context, p *prepared, gc gateCall, requestID string) (verdict, error) {
	requested := event.ApprovalRequested{RequestID: requestID, CallID: p.call.ID, Summary: summary(gc), Class: classString(p.class)}
	if p.class == tool.ClassExec && gc.command != "" {
		if prefix, ok := rules.GrantPrefix(gc.command); ok {
			requested.GrantPrefix = prefix
		}
	}
	if _, err := w.t.appendEvents(ctx, requested); err != nil {
		return verdict{}, err
	}
	v := w.decide(ctx, gc, requestID)
	resolved := event.ApprovalResolved{RequestID: requestID, Decision: v.decision, By: v.by, Reason: v.reason, Scores: v.scores}
	if _, err := w.t.appendEvents(ctx, resolved); err != nil {
		return verdict{}, err
	}
	return v, nil
}

type inBandGuard struct {
	p      *prepared
	gc     gateCall
	denial *inBandDenial
}

func (w *worker) prepareGuard(ctx context.Context, toolCall event.ToolCall, requestID string) (inBandGuard, error) {
	p, denial, err := w.prepareInBand(toolCall)
	if err != nil || denial != nil {
		return inBandGuard{denial: denial}, err
	}
	by := "mode:" + string(w.settings.Approval)
	gc, gateErr := w.gateCall(ctx, p, requestID)
	if gateErr != nil {
		p.discardPlan()
		return inBandGuard{gc: gc, denial: &inBandDenial{p.class, summary(gc), decline(by, gateErr.Error())}}, nil
	}
	if err := w.journalCover(ctx, toolCall); err != nil {
		p.discardPlan()
		return inBandGuard{}, err
	}
	return inBandGuard{p: p, gc: gc}, nil
}

func (w *worker) prepareInBand(toolCall event.ToolCall) (*prepared, *inBandDenial, error) {
	by := "mode:" + string(w.settings.Approval)
	item, ok := w.tools.byName[toolCall.Name]
	if !ok {
		return nil, &inBandDenial{tool.ClassRemote, "", decline(by, "Tool "+toolCall.Name+" not found")}, nil
	}
	if err := w.t.ext.Direct(toolCall.Name); err != nil {
		return nil, &inBandDenial{item.Class(), "", decline(by, err.Error())}, nil
	}
	args, repairs, fail := repairArgs(item.Spec(), toolCall.Args)
	if fail != "" {
		return nil, &inBandDenial{item.Class(), "", decline(by, "invalid arguments for "+toolCall.Name+": "+fail)}, nil
	}
	repeated, err := w.repeated(toolCall.Name, args)
	if err != nil {
		return nil, nil, err
	}
	if repeated {
		return nil, &inBandDenial{item.Class(), "", decline("rule:loop-guard", loopGuardMessage)}, nil
	}
	return &prepared{call: toolCall, tool: item, class: item.Class(), args: args, repairs: repairs}, nil, nil
}

func (w *worker) executeInBand(ctx context.Context, p *prepared) (event.ToolCompleted, error) {
	completed, err := w.execute(ctx, p)
	if err != nil {
		p.discardPlan()
		return event.ToolCompleted{}, err
	}
	if _, err := w.t.appendEvents(context.WithoutCancel(ctx), completed); err != nil {
		return event.ToolCompleted{}, err
	}
	w.inBand.put(p.call, nil)
	return completed, nil
}

func (w *worker) denyInBand(ctx context.Context, call event.ToolCall, requestID string, denial inBandDenial) (event.ToolCompleted, error) {
	if err := w.journalCover(ctx, call); err != nil {
		return event.ToolCompleted{}, err
	}
	requested := event.ApprovalRequested{RequestID: requestID, CallID: call.ID, Summary: truncate(cmp.Or(denial.text, call.Name), summaryLimit), Class: classString(denial.class)}
	if _, err := w.t.appendEvents(ctx, requested); err != nil {
		return event.ToolCompleted{}, err
	}
	resolved := event.ApprovalResolved{RequestID: requestID, Decision: denial.verdict.decision, By: denial.verdict.by, Reason: denial.verdict.reason}
	if _, err := w.t.appendEvents(ctx, resolved); err != nil {
		return event.ToolCompleted{}, err
	}
	return w.completeInBandDeny(ctx, call, denial.verdict)
}

func (w *worker) completeInBandDeny(ctx context.Context, call event.ToolCall, v verdict) (event.ToolCompleted, error) {
	completed := event.ToolCompleted{CallID: call.ID, Output: v.reason, Recovered: "none"}
	if _, err := w.t.appendEvents(ctx, completed); err != nil {
		return event.ToolCompleted{}, err
	}
	w.inBand.put(call, nil)
	return completed, nil
}

func (w *worker) journalInBand(ctx context.Context, calls []event.ToolCall) ([]event.ToolCall, error) {
	pending := make([]event.ToolCall, 0, len(calls))
	var events []event.Event
	for _, call := range calls {
		recorded, ok := w.inBand.take(call.ID)
		if !ok || !call.Resolved {
			pending = append(pending, call)
			continue
		}
		events = append(events, recorded...)
	}
	_, err := w.t.appendEvents(ctx, events...)
	return pending, err
}

func (w *worker) flushInBand(ctx context.Context, head uint64) {
	if w.inBand == nil {
		return
	}
	calls, events := w.inBand.drain()
	if len(calls) == 0 || len(events) == 0 {
		return
	}
	message := event.AssistantMessage{ItemID: newID("item-"), TurnID: w.turnID, ToolCalls: calls, StopReason: "tool_calls", ContextSeq: head}
	if _, err := w.t.appendEvents(ctx, append([]event.Event{message}, events...)...); err != nil {
		w.t.host.log.Warn("agent: journal in-band calls after stream failure", "thread", w.t.id, "error", err)
	}
}
