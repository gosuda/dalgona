package agent

import (
	"cmp"
	"context"
	"maps"
	"slices"
	"sync"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/agent/rules"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/cursor"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

type inBandCalls struct {
	stateMu sync.Mutex
	calls   map[string]event.ToolCall
	events  map[string][]event.Event
	cancel  bool
	request uint64
	owners  map[string]*invocationOwner
	reviews *reviewRound
}

func newInBandCalls() *inBandCalls {
	return &inBandCalls{
		calls: make(map[string]event.ToolCall), events: make(map[string][]event.Event),
		owners: make(map[string]*invocationOwner),
	}
}

func (c *inBandCalls) begin(request uint64) {
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	c.request = request
	c.owners = make(map[string]*invocationOwner)
}

func (c *inBandCalls) requestSeq() uint64 {
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	return c.request
}

func (c *inBandCalls) own(claim invocationClaim) (*invocationOwner, bool) {
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	if owner, held := c.owners[claim.identity]; held {
		return owner, true
	}
	owner := &invocationOwner{fingerprint: claim.fingerprint, done: make(chan struct{})}
	c.owners[claim.identity] = owner
	return owner, false
}

func (c *inBandCalls) reviewRound() *reviewRound {
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	if c.reviews == nil {
		c.reviews = &reviewRound{}
	}
	return c.reviews
}

func (c *inBandCalls) takeReviews() *reviewRound {
	if c == nil {
		return nil
	}
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	round := c.reviews
	c.reviews = nil
	return round
}

func (c *inBandCalls) put(call event.ToolCall, events []event.Event) {
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	c.calls[call.ID] = call
	c.events[call.ID] = events
}

func (c *inBandCalls) take(callID string) ([]event.Event, bool) {
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	_, resolved := c.calls[callID]
	events := c.events[callID]
	delete(c.events, callID)
	return events, resolved
}

func (c *inBandCalls) drain() ([]event.ToolCall, []event.Event) {
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
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
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	c.cancel = true
}

func (c *inBandCalls) takeCancel() bool {
	if c == nil {
		return false
	}
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	cancelled := c.cancel
	c.cancel = false
	return cancelled
}

func (c *inBandCalls) resolved(callID string) bool {
	c.stateMu.Lock()
	defer c.stateMu.Unlock()
	_, ok := c.calls[callID]
	return ok
}

func (w *worker) interruptTurn(ctx context.Context) {
	_, _ = ask(ctx, w.t, func(a *actor) bool { a.interrupt(); return true })
}

func (w *worker) openStream(ctx context.Context, providerID string, req model.Request, contextSeq uint64) (model.Stream, error) {
	if w.inBand == nil {
		w.inBand = newInBandCalls()
	}
	w.inBand.begin(contextSeq)
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
	return model.ToolResult{ID: call.ID, Name: call.Name, Output: jsonString(completed.Output), IsError: !completed.OK}, nil
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
	p, denial := w.prepareInBand(toolCall)
	if denial != nil {
		return w.denyInBand(ctx, toolCall, requestID, *denial)
	}
	return w.dispatchInBand(ctx, p, requestID)
}

func (w *worker) dispatchInBand(ctx context.Context, p *prepared, requestID string) (event.ToolCompleted, error) {
	p.evidence = snapshot.Evidence{Consumer: snapshot.ConsumerModel, Cutoff: w.inBand.requestSeq()}
	profile, ok := profileOf(p.tool)
	if !ok {
		return w.runInBandCall(ctx, p, requestID)
	}
	claim := w.claimFor(p, profile, p.evidence.Cutoff)
	owner, held := w.inBand.own(*claim)
	if held {
		return owner.join(ctx, *claim, p.call.ID)
	}
	p.claim = claim
	completed, err := w.runInBandCall(ctx, p, requestID)
	owner.finish(completed, err)
	return completed, err
}

func (w *worker) runInBandCall(ctx context.Context, p *prepared, requestID string) (event.ToolCompleted, error) {
	repeated, err := w.repeated(ctx, p.call.Name, p.args)
	if err != nil {
		return event.ToolCompleted{}, err
	}
	if repeated {
		return w.denyInBand(ctx, p.call, requestID, inBandDenial{p.class, "", decline("rule:loop-guard", loopGuardMessage)})
	}
	guard, err := w.guardInBand(ctx, p, requestID)
	if err != nil {
		return event.ToolCompleted{}, err
	}
	if guard.denial != nil {
		return w.denyInBand(ctx, p.call, requestID, *guard.denial)
	}
	v, err := w.approveInBand(ctx, p, guard.gc, requestID)
	if err != nil {
		p.discardPlan()
		return event.ToolCompleted{}, err
	}
	return w.resolveInBand(ctx, p, p.call, v)
}

func (w *worker) resolveInBand(ctx context.Context, p *prepared, toolCall event.ToolCall, v verdict) (event.ToolCompleted, error) {
	if v.cancel {
		w.inBand.markCancel()
	}
	if !v.allow {
		p.discardPlan()
		v.meta = refusalMeta(p.tool, declineOutcome(v))
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
	cover := event.AssistantMessage{
		ItemID: newID("item-"), TurnID: w.turnID, ToolCalls: []event.ToolCall{toolCall},
		StopReason: "tool_calls", ContextSeq: w.inBand.requestSeq(),
	}
	_, err := w.t.appendEvents(ctx, cover)
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
	gc     gateCall
	denial *inBandDenial
}

func (w *worker) guardInBand(ctx context.Context, p *prepared, requestID string) (inBandGuard, error) {
	by := "mode:" + string(w.settings.Approval)
	gc, gateErr := w.gateCall(ctx, p, requestID)
	if gateErr != nil {
		p.discardPlan()
		refusal := decline(by, gateErr.Error())
		refusal.meta = planRefusalMeta(gateErr)
		return inBandGuard{gc: gc, denial: &inBandDenial{p.class, summary(gc), refusal}}, nil
	}
	if err := w.journalCover(ctx, p.call); err != nil {
		p.discardPlan()
		return inBandGuard{}, err
	}
	return inBandGuard{gc: gc}, nil
}

func (w *worker) prepareInBand(toolCall event.ToolCall) (*prepared, *inBandDenial) {
	by := "mode:" + string(w.settings.Approval)
	item, ok := w.tools.byName[toolCall.Name]
	if !ok {
		return nil, &inBandDenial{tool.ClassRemote, "", decline(by, "Tool "+toolCall.Name+" not found")}
	}
	if refusal, refused := w.readOnlyRefusal(item.Class(), toolCall.Name); refused {
		return nil, &inBandDenial{item.Class(), "", refusal}
	}
	if err := w.t.ext.Direct(toolCall.Name); err != nil {
		return nil, &inBandDenial{item.Class(), "", decline(by, err.Error())}
	}
	args, repairs, fail := repairArgs(item.Spec(), toolCall.Args)
	if fail != "" {
		return nil, &inBandDenial{item.Class(), "", decline(by, "invalid arguments for "+toolCall.Name+": "+fail)}
	}
	return &prepared{call: toolCall, tool: item, class: item.Class(), args: args, repairs: repairs}, nil
}

func (w *worker) executeInBand(ctx context.Context, p *prepared) (event.ToolCompleted, error) {
	completed, err := w.execute(ctx, p)
	if err != nil {
		p.discardPlan()
		return event.ToolCompleted{}, err
	}
	head, err := w.t.appendEvents(context.WithoutCancel(ctx), completed)
	if err != nil {
		return event.ToolCompleted{}, err
	}
	w.deliver(completed, head)
	w.inBand.put(p.call, nil)
	w.queueReview(w.turnCtx, w.inBand.reviewRound(), p, completed)
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
	masked, err := w.t.host.mask(denial.verdict.reason)
	if err != nil {
		return event.ToolCompleted{}, err
	}
	resolved := event.ApprovalResolved{RequestID: requestID, Decision: denial.verdict.decision, By: denial.verdict.by, Reason: masked}
	if _, err := w.t.appendEvents(ctx, resolved); err != nil {
		return event.ToolCompleted{}, err
	}
	return w.completeInBandDeny(ctx, call, denial.verdict)
}

func (w *worker) completeInBandDeny(ctx context.Context, call event.ToolCall, v verdict) (event.ToolCompleted, error) {
	masked, err := w.t.host.mask(v.reason)
	if err != nil {
		return event.ToolCompleted{}, err
	}
	completed := event.ToolCompleted{CallID: call.ID, Output: masked, Recovered: "none", Meta: v.meta}
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
		if !call.Resolved {
			pending = append(pending, call)
			continue
		}
		recorded, ok := w.inBand.take(call.ID)
		if !ok {
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
	w.journalStranded(ctx, head)
	if err := w.joinReviews(ctx); err != nil {
		w.t.host.log.Warn("agent: journal in-band edit reviews", "thread", w.t.id, "error", err)
	}
}

func (w *worker) journalStranded(ctx context.Context, head uint64) {
	calls, events := w.inBand.drain()
	if len(calls) == 0 || len(events) == 0 {
		return
	}
	message := event.AssistantMessage{ItemID: newID("item-"), TurnID: w.turnID, ToolCalls: calls, StopReason: "tool_calls", ContextSeq: head}
	if _, err := w.t.appendEvents(ctx, append([]event.Event{message}, events...)...); err != nil {
		w.t.host.log.Warn("agent: journal in-band calls after stream failure", "thread", w.t.id, "error", err)
	}
}
