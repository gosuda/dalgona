package agent

import (
	"cmp"
	"context"
	"strings"

	"github.com/gosuda/dalgona/internal/agent/event"
)

const scriptPrompt = "dalgona run "

type ScriptResumer interface {
	ResumeScript(ctx context.Context, runID string) error
}

func (t *Thread) RunScript(ctx context.Context, entry string, run func(context.Context) error) error {
	done := make(chan error, 1)
	if err := askErr(t, func(a *actor) error { return a.startScript(scriptPrompt+entry, run, done) }); err != nil {
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

func (a *actor) startScript(text string, run func(context.Context) error, done chan<- error) error {
	if a.state.Archived {
		return archivedError(a.t.id)
	}
	if a.busy {
		return runningError(a.t.id)
	}
	masked, blob, err := a.t.host.textPayload(a.t.host.mask(text))
	if err != nil {
		return err
	}
	turnID := newID("turn-")
	message := event.UserMessage{ItemID: newID("item-"), TurnID: turnID, Text: masked, TextBlob: blob, Kind: string(InputPrompt), Verbatim: true}
	if _, err := a.append(event.TurnStarted{TurnID: turnID}, message); err != nil {
		return err
	}
	a.autoContinue = false
	a.spawn(func(ctx context.Context) { done <- (&worker{t: a.t, turnID: turnID}).runScript(ctx, run) })
	return nil
}

func (w *worker) runScript(ctx context.Context, run func(context.Context) error) error {
	v, err := w.t.view()
	if err == nil {
		err = w.use(v)
	}
	if err == nil {
		err = w.openRegistry()
	}
	if err == nil {
		err = run(withWorker(ctx, w))
	}
	o := outcome{status: statusCompleted, final: true}
	if err != nil {
		o = failure(ctx, err)
		o.final = true
	}
	w.end(ctx, o)
	return err
}

func (w *worker) recoverScriptTurn(ctx context.Context) bool {
	v, err := w.t.view()
	if err != nil || !w.scriptTurn(v) {
		return false
	}
	resumer, resumable := w.t.ext.(ScriptResumer)
	if _, pinned := v.state.Scripts[w.t.id]; !resumable || !pinned {
		w.end(ctx, outcome{status: settledScriptStatus(v, w.t.id), final: true})
		return true
	}
	err = w.runScript(ctx, func(ctx context.Context) error {
		if nestedErr := w.recoverNested(ctx, v, w.t.id+"/"); nestedErr != nil {
			return nestedErr
		}
		return resumer.ResumeScript(ctx, w.t.id)
	})
	if err != nil {
		w.t.host.log.Warn("agent: resume script turn", "thread", w.t.id, "turn", w.turnID, "error", err)
	}
	return true
}

func settledScriptStatus(v view, runID string) string {
	for _, item := range v.records {
		if done, ok := item.Event.(event.ScriptCompleted); ok && done.RunID == runID {
			return statusCompleted
		}
	}
	return statusInterrupted
}

func (w *worker) scriptTurn(v view) bool {
	script := false
	for _, item := range v.records {
		switch value := item.Event.(type) {
		case event.AssistantMessage:
			if value.TurnID == w.turnID {
				return false
			}
		case event.UserMessage:
			if value.TurnID == w.turnID && value.Kind == string(InputPrompt) && value.Verbatim {
				text, err := w.t.host.loadText(value.Text, value.TextBlob)
				script = err == nil && strings.HasPrefix(text, scriptPrompt)
			}
		}
	}
	return script
}
