package agent

import (
	"context"
	"fmt"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/model"
)

const (
	statusCompleted   = "completed"
	statusInterrupted = "interrupted"
	statusFailed      = "failed"
	hookTimeout       = 30 * time.Second
)

type worker struct {
	t            *Thread
	turnID       string
	continuation bool
	registry     *model.Registry
}

type outcome struct {
	status  string
	failure string
	head    uint64
}

func (a *actor) spawn(job func(ctx context.Context)) {
	ctx, cancel := context.WithCancelCause(context.Background())
	a.busy, a.cancel = true, cancel
	a.t.workers.Go(func() { job(ctx) })
}

func (a *actor) spawnTurn(turnID string) {
	a.spawn(func(ctx context.Context) { (&worker{t: a.t, turnID: turnID}).runTurn(ctx) })
}

func (a *actor) spawnContinuation(turnID string) {
	a.spawn(func(ctx context.Context) { (&worker{t: a.t, turnID: turnID, continuation: true}).runTurn(ctx) })
}

func (t *Thread) startRecovery() {
	if err := t.do(func(a *actor) {
		turnID := a.state.TurnID
		a.spawn(func(ctx context.Context) { (&worker{t: t, turnID: turnID}).recoverTurn(ctx) })
	}); err != nil {
		t.host.log.Warn("agent: recovery not started", "thread", t.id, "error", err)
	}
}

func (w *worker) release() {
	if err := w.t.do(func(a *actor) { a.idle() }); err != nil {
		w.t.host.log.Debug("agent: release after close", "thread", w.t.id)
	}
}

func (a *actor) idle() {
	if a.cancel != nil {
		a.cancel(nil)
	}
	a.busy, a.cancel = false, nil
}

func (w *worker) runTurn(ctx context.Context) {
	if err := w.openRegistry(); err != nil {
		w.end(ctx, outcome{status: statusFailed, failure: err.Error()})
		return
	}
	w.drive(ctx)
}

func (w *worker) drive(ctx context.Context) {
	for {
		if w.end(ctx, w.loop(ctx)) {
			return
		}
	}
}

func (w *worker) openRegistry() error {
	registry, err := w.t.host.cfg.Providers(w.onRetry)
	if err != nil {
		return fmt.Errorf("agent: providers: %w", err)
	}
	w.registry = registry
	return nil
}

func (w *worker) onRetry(attempt int, verdict model.Verdict, status int, delay time.Duration) {
	name := "retry"
	if verdict == model.Overflow {
		name = "overflow"
	}
	retry := event.ModelRetry{TurnID: w.turnID, Attempt: attempt, Verdict: name, Status: status, DelayMS: delay.Milliseconds()}
	if _, err := w.t.appendEvents(context.Background(), retry); err != nil {
		w.t.host.log.Warn("agent: journal model.retry", "thread", w.t.id, "error", err)
	}
}

func (w *worker) loop(ctx context.Context) outcome {
	waited := false
	for {
		if ctx.Err() != nil {
			return outcome{status: statusInterrupted}
		}
		v, messages, err := w.context()
		if err != nil {
			return failure(ctx, err)
		}
		if !needsModel(messages) {
			if waited {
				return outcome{status: statusCompleted, head: v.head()}
			}
			w.boundary(ctx, BoundaryAgentEnd, v.head())
			waited = true
			continue
		}
		waited = false
		if err := w.step(ctx, v, messages); err != nil {
			return failure(ctx, err)
		}
	}
}

func (w *worker) context() (view, []model.Message, error) {
	v, err := w.t.view()
	if err != nil {
		return v, nil, err
	}
	messages, err := w.t.messages(v)
	return v, messages, err
}

func failure(ctx context.Context, err error) outcome {
	if ctx.Err() != nil {
		return outcome{status: statusInterrupted}
	}
	return outcome{status: statusFailed, failure: err.Error()}
}

func (w *worker) step(ctx context.Context, v view, messages []model.Message) error {
	compacted, err := w.autoCompact(ctx, v)
	if err != nil || compacted {
		return err
	}
	reply, err := w.callModel(ctx, v, messages)
	if err != nil {
		return err
	}
	if err = w.journalAssistant(ctx, reply); err != nil {
		return err
	}
	if len(reply.calls) == 0 {
		return nil
	}
	if err = w.runCalls(ctx, reply.calls); err != nil {
		return err
	}
	v, err = w.t.view()
	if err != nil {
		return err
	}
	w.boundary(ctx, BoundaryTurn, v.head())
	return nil
}

func (w *worker) boundary(ctx context.Context, kind BoundaryKind, head uint64) {
	hook, err := ask(w.t, func(a *actor) BoundaryHook { return a.hook })
	if err != nil {
		w.t.host.log.Debug("agent: boundary skipped on a closed thread", "thread", w.t.id, "error", err)
		return
	}
	if hook == nil {
		return
	}
	wait := hook(Boundary{Thread: w.t.id, Turn: w.turnID, Kind: kind, Head: head, Continuation: w.continuation})
	if wait == nil {
		return
	}
	if err := wait(ctx); err != nil {
		w.t.host.log.Warn("agent: boundary wait released with an error", "thread", w.t.id, "turn", w.turnID, "error", err)
	}
}

func (w *worker) end(ctx context.Context, o outcome) bool {
	ended, err := ask(w.t, func(a *actor) bool { return a.finishTurn(w.turnID, o) })
	if err != nil {
		w.t.host.log.Warn("agent: turn ended on a closed thread", "thread", w.t.id, "turn", w.turnID, "error", err)
		return true
	}
	if !ended {
		return false
	}
	if o.status != statusCompleted {
		w.announceEnd(o.status)
	}
	w.notifyTurnEnd(ctx, o.status)
	return true
}

type hookSnapshot struct {
	hook BoundaryHook
	head uint64
}

func (w *worker) announceEnd(status string) {
	snap, err := ask(w.t, func(a *actor) hookSnapshot { return hookSnapshot{hook: a.hook, head: a.head()} })
	if err != nil {
		w.t.host.log.Debug("agent: end boundary skipped on a closed thread", "thread", w.t.id, "error", err)
		return
	}
	if snap.hook == nil {
		return
	}
	if wait := snap.hook(Boundary{Thread: w.t.id, Turn: w.turnID, Kind: BoundaryAgentEnd, Head: snap.head, Ended: status}); wait != nil {
		w.t.host.log.Debug("agent: ignoring a wait for an ended turn", "thread", w.t.id, "turn", w.turnID)
	}
}

func (a *actor) finishTurn(turnID string, o outcome) bool {
	if o.status == statusCompleted && a.newInputSince(o.head) {
		return false
	}
	a.settle(o.status)
	completed := event.TurnCompleted{TurnID: turnID, Status: o.status, Error: o.failure}
	if _, err := a.append(completed); err != nil {
		a.t.host.log.Error("agent: journal turn.completed", "thread", a.t.id, "turn", turnID, "error", err)
	}
	clear(a.approvals)
	a.idle()
	return true
}

func (a *actor) newInputSince(head uint64) bool {
	for _, item := range a.records[min(head, a.head()):] {
		switch value := item.Event.(type) {
		case event.UserMessage:
			return true
		case event.AdvisorNote:
			if value.Delivery != string(DeliveryAside) {
				return true
			}
		}
	}
	return false
}

func (a *actor) settle(status string) {
	reason := "the turn was interrupted"
	if status == statusFailed {
		reason = "the turn failed"
	}
	events := make([]event.Event, 0, len(a.state.Approvals)+len(a.state.InFlight)+len(a.state.PendingCalls))
	for requestID := range a.state.Approvals {
		events = append(events, event.ApprovalResolved{RequestID: requestID, Decision: "cancel", By: "user", Reason: reason})
	}
	for id := range a.state.InFlight {
		events = append(events, event.ToolCompleted{CallID: id, Output: "interrupted: " + reason, Recovered: "interrupted"})
	}
	for id := range a.state.PendingCalls {
		events = append(events, event.ToolCompleted{CallID: id, Output: "skipped: " + reason, Recovered: "skipped"})
	}
	if _, err := a.append(events...); err != nil {
		a.t.host.log.Error("agent: settle unfinished calls", "thread", a.t.id, "error", err)
	}
}

func (w *worker) notifyTurnEnd(ctx context.Context, status string) {
	for _, hook := range w.t.host.cfg.Hooks {
		hookCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), hookTimeout)
		_, err := hook.Handle(hookCtx, HookEvent{Name: "turn_end", Thread: w.t.id, Turn: w.turnID, Status: status})
		cancel()
		if err != nil {
			w.t.host.log.Warn("agent: turn_end hook", "thread", w.t.id, "error", err)
		}
	}
}
