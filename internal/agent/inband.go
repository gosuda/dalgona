package agent

import (
	"cmp"
	"context"
	"sync"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/rules"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/cursor"
	"github.com/gosuda/dalgona/internal/tool"
)

type inBandCalls struct {
	mu     sync.Mutex
	events map[string][]event.Event
	cancel bool
}

func (c *inBandCalls) put(callID string, events []event.Event) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.events[callID] = events
}

func (c *inBandCalls) take(callID string) ([]event.Event, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	events, ok := c.events[callID]
	delete(c.events, callID)
	return events, ok
}

func (c *inBandCalls) markCancel() {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.cancel = true
}

func (c *inBandCalls) takeCancel() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	cancelled := c.cancel
	c.cancel = false
	return cancelled
}

func (w *worker) interruptTurn() {
	_, _ = ask(w.t, func(a *actor) bool { a.interrupt(); return true })
}

func (w *worker) openStream(ctx context.Context, providerID string, req model.Request) (model.Stream, error) {
	w.inBand = &inBandCalls{events: make(map[string][]event.Event)}
	provider, ok := w.registry.Provider(providerID)
	if native, inBand := provider.(*cursor.Provider); ok && inBand {
		return native.StreamWith(ctx, req, cursor.StreamOptions{InBand: w.runInBand})
	}
	return w.registry.Stream(ctx, providerID, req)
}

func (w *worker) runInBand(ctx context.Context, call model.ToolCall) (model.ToolResult, error) {
	events, completed, err := w.inBandEvents(ctx, event.ToolCall{ID: call.ID, Name: call.Name, Args: call.Args, Freeform: call.Freeform})
	if err != nil {
		return model.ToolResult{}, err
	}
	w.inBand.put(call.ID, events)
	return model.ToolResult{ID: call.ID, Name: call.Name, Output: quoteJSON(completed.Output), IsError: !completed.OK}, nil
}

func (w *worker) inBandEvents(ctx context.Context, call event.ToolCall) ([]event.Event, event.ToolCompleted, error) {
	requestID := newID("req-")
	by := "mode:" + string(w.settings.Approval)
	item, ok := w.tools.byName[call.Name]
	if !ok {
		return declinedInBand(call, requestID, tool.ClassRemote, "", decline(by, "Tool "+call.Name+" not found"))
	}
	args, repairs, err := repairArgs(item.Spec(), call.Args)
	if err != nil {
		return declinedInBand(call, requestID, item.Class(), "", decline(by, "invalid arguments for "+call.Name+": "+err.Error()))
	}
	p := &prepared{call: call, tool: item, class: item.Class(), args: args, repairs: repairs}
	gc, err := w.gateCall(ctx, p, requestID)
	if err != nil {
		return declinedInBand(call, requestID, p.class, summary(gc), decline(by, err.Error()))
	}
	v := w.decide(ctx, gc, requestID)
	if !v.allow {
		events, completed, err := declinedInBand(call, requestID, p.class, summary(gc), v)
		if err != nil {
			return nil, event.ToolCompleted{}, err
		}
		if v.cancel {
			w.inBand.markCancel()
		}
		return events, completed, nil
	}
	p.verdict = v
	return w.executeInBand(ctx, p, gc, requestID)
}

func (w *worker) executeInBand(ctx context.Context, p *prepared, gc gateCall, requestID string) ([]event.Event, event.ToolCompleted, error) {
	var files []tool.FileChange
	if p.plan != nil {
		files = p.plan.Files
	}
	started, err := w.started(p, files)
	if err != nil {
		return nil, event.ToolCompleted{}, err
	}
	completed, err := w.execute(ctx, p)
	if err != nil {
		return nil, event.ToolCompleted{}, err
	}
	v := p.verdict
	requested := event.ApprovalRequested{RequestID: requestID, CallID: p.call.ID, Summary: summary(gc), Class: classString(p.class)}
	if p.class == tool.ClassExec && gc.command != "" {
		if prefix, ok := rules.GrantPrefix(gc.command); ok {
			requested.GrantPrefix = prefix
		}
	}
	events := []event.Event{
		requested,
		event.ApprovalResolved{RequestID: requestID, Decision: v.decision, By: v.by, Reason: v.reason, Scores: v.scores},
		started, completed,
	}
	return events, completed, nil
}

func declinedInBand(call event.ToolCall, requestID string, class tool.Class, text string, v verdict) ([]event.Event, event.ToolCompleted, error) {
	completed := event.ToolCompleted{CallID: call.ID, Output: v.reason, Recovered: "none"}
	return []event.Event{
		event.ApprovalRequested{RequestID: requestID, CallID: call.ID, Summary: truncate(cmp.Or(text, call.Name), summaryLimit), Class: classString(class)},
		event.ApprovalResolved{RequestID: requestID, Decision: v.decision, By: v.by, Reason: v.reason},
		completed,
	}, completed, nil
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
