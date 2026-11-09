package agent

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/config"
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
	inBand       *inBandCalls
	settings     Settings
	tools        *toolset
}

func pathSettings(created event.SessionCreated, records []event.Seqd) Settings {
	out := Settings{Model: created.Model, Approval: config.Approval(created.Approval), Sandbox: config.SandboxMode(created.Sandbox), Effort: config.Reasoning(created.Effort)}
	for _, item := range event.ActivePath(records) {
		if message, ok := item.Event.(event.UserMessage); ok {
			out.Model = cmp.Or(message.Model, out.Model)
			out.Approval = cmp.Or(config.Approval(message.Approval), out.Approval)
			out.Sandbox = cmp.Or(config.SandboxMode(message.Sandbox), out.Sandbox)
		}
	}
	return out
}

func (w *worker) use(v view) error {
	w.settings = pathSettings(w.t.created, v.records)
	tools, err := w.t.toolsFor(w.settings.Model)
	if err != nil {
		return err
	}
	w.tools = tools
	return nil
}

type outcome struct {
	status  string
	failure string
	head    uint64
	final   bool
}

var (
	errYielded     = errors.New("agent: the subagent yielded")
	errHookTimeout = errors.New("agent: a turn_end hook ran past " + hookTimeout.String())
)

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
	if err := w.prime(); err != nil {
		w.end(ctx, outcome{status: statusFailed, failure: err.Error()})
		return
	}
	w.drive(ctx)
}

func (w *worker) prime() error {
	v, err := w.t.view()
	if err != nil {
		return err
	}
	if err := w.use(v); err != nil {
		return err
	}
	return w.openRegistry()
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
		err = w.step(ctx, v, messages)
		if errors.Is(err, errYielded) {
			return outcome{status: statusCompleted, final: true}
		}
		if err != nil {
			return failure(ctx, err)
		}
	}
}

func (w *worker) context() (view, []model.Message, error) {
	v, err := w.t.view()
	if err != nil {
		return v, nil, err
	}
	if err = w.use(v); err != nil {
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
		w.flushInBand(context.WithoutCancel(ctx), v.head())
		return err
	}
	if err = w.journalAssistant(ctx, reply); err != nil {
		return err
	}
	if len(reply.calls) == 0 {
		return nil
	}
	return w.callRound(ctx, reply.calls)
}

func (w *worker) callRound(ctx context.Context, calls []event.ToolCall) error {
	pending, err := w.journalInBand(ctx, calls)
	if err != nil {
		return err
	}
	if w.inBand.takeCancel() {
		w.interruptTurn()
	}
	if err = w.runCalls(ctx, pending); err != nil {
		return err
	}
	v, err := w.t.view()
	if err != nil {
		return err
	}
	if yieldedIn(v, calls) {
		return errYielded
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
	decided, err := ask(w.t, func(a *actor) endDecision {
		return endDecision{settle: !a.continues(o), head: a.head()}
	})
	if err != nil {
		w.t.host.log.Warn("agent: turn ended on a closed thread", "thread", w.t.id, "turn", w.turnID, "error", err)
		return true
	}
	if decided.settle {
		w.notifyTurnEnd(ctx, o.status)
	}
	ended, err := ask(w.t, func(a *actor) bool {
		if !decided.settle {
			return false
		}
		return a.settleTurn(w.turnID, o, decided.head)
	})
	if err != nil {
		w.t.host.log.Warn("agent: turn ended on a closed thread", "thread", w.t.id, "turn", w.turnID, "error", err)
		return true
	}
	if ended && o.status != statusCompleted {
		w.announceEnd(o.status)
	}
	return ended
}

type endDecision struct {
	settle bool
	head   uint64
}

func (a *actor) settleTurn(turnID string, o outcome, head uint64) bool {
	a.settle(o.status)
	completed := event.TurnCompleted{TurnID: turnID, Status: o.status, Error: o.failure}
	if _, err := a.append(completed); err != nil {
		a.t.host.log.Error("agent: journal turn.completed", "thread", a.t.id, "turn", turnID, "error", err)
	}
	clear(a.approvals)
	a.idle()
	if o.status == statusCompleted && a.newInputSince(head) {
		next := newID("turn-")
		if _, err := a.append(event.TurnStarted{TurnID: next}); err == nil {
			a.spawnContinuation(next)
		}
	}
	return true
}

func (a *actor) continues(o outcome) bool {
	return o.status == statusCompleted && !o.final && a.newInputSince(o.head)
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
	for run := range a.state.Scripts {
		events = append(events, event.ScriptCompleted{RunID: run, Error: "cancelled: " + reason})
	}
	if _, err := a.append(events...); err != nil {
		a.t.host.log.Error("agent: settle unfinished calls", "thread", a.t.id, "error", err)
	}
}

func (w *worker) notifyTurnEnd(ctx context.Context, status string) {
	hookCtx, cancel := context.WithTimeoutCause(withWorker(context.WithoutCancel(ctx), w), hookTimeout, errHookTimeout)
	defer cancel()
	if err := w.t.ext.TurnEnd(hookCtx, w.turnID, status); err != nil {
		w.t.host.log.Warn("agent: turn_end hook", "thread", w.t.id, "error", err)
	}
}
