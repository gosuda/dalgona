package agent

import (
	"cmp"
	"context"
	"errors"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
)

type ExternalTurnResumer interface {
	MatchExternalTurn(ctx context.Context, turnID string) (bool, error)
	ResumeExternalTurn(ctx context.Context) error
}

var ErrExternalTurnInterrupted = errors.New("agent: external turn interrupted")

func (t *Thread) RunExternalTurn(ctx context.Context, text string, run func(context.Context) error) error {
	done := make(chan error, 1)
	if err := askErr(ctx, t, func(a *actor) error { return a.startExternalTurn(ctx, text, run, done) }); err != nil {
		return err
	}
	select {
	case err := <-done:
		return err
	case <-ctx.Done():
		t.Interrupt()
		return cmp.Or(<-done, context.Cause(ctx))
	}
}

func (a *actor) startExternalTurn(ctx context.Context, text string, run func(context.Context) error, done chan<- error) error {
	if a.state.Archived {
		return archivedError(a.t.id)
	}
	if a.busy {
		return runningError(a.t.id)
	}
	maskedText, err := a.t.host.mask(text)
	if err != nil {
		return err
	}
	masked, blob, err := a.t.host.textPayload(maskedText)
	if err != nil {
		return err
	}
	turnID := newID("turn-")
	message := event.UserMessage{ItemID: newID("item-"), TurnID: turnID, Text: masked, TextBlob: blob, Kind: string(InputPrompt), Verbatim: true}
	if _, err := a.append(ctx, event.TurnStarted{TurnID: turnID}, message); err != nil {
		return err
	}
	a.autoContinue = false
	a.spawn(func(ctx context.Context) { done <- (&worker{t: a.t, turnID: turnID}).runExternalTurn(ctx, run) })
	return nil
}

func (w *worker) runExternalTurn(ctx context.Context, run func(context.Context) error) error {
	v, err := w.t.view(ctx)
	if err == nil {
		err = w.use(ctx, v)
	}
	if err == nil {
		err = w.openRegistry()
	}
	if err == nil {
		err = run(withWorker(ctx, w))
	}
	o := outcome{status: statusCompleted, final: true}
	if err != nil {
		o = failedOutcome(ctx, err)
		o.final = true
	}
	w.end(ctx, o)
	return err
}

func (w *worker) recoverExternalTurn(ctx context.Context) bool {
	resumer, ok := w.t.ext.(ExternalTurnResumer)
	if !ok {
		return false
	}
	match, err := resumer.MatchExternalTurn(ctx, w.turnID)
	if err != nil {
		w.end(ctx, outcome{status: statusFailed, failure: err.Error(), final: true})
		return true
	}
	if !match {
		return false
	}
	err = w.runExternalTurn(ctx, func(ctx context.Context) error {
		return resumer.ResumeExternalTurn(ctx)
	})
	if err != nil {
		w.t.host.log.Warn("agent: resume external turn", "thread", w.t.id, "turn", w.turnID, "error", err)
	}
	return true
}
