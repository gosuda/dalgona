package scripthost

import (
	"context"
	"strings"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	corescript "github.com/gosuda/dalgona/internal/script"
	corescripthost "github.com/gosuda/dalgona/internal/scripthost"

	"github.com/gosuda/dalgona/internal/dalgona/script"
)

const externalPrompt = "dalgona run "

func RunWorkflow(ctx context.Context, t *agent.Thread, cfg Config, w script.Workflow) (corescript.Outcome, error) {
	fx := corescripthost.NewEffects(t, cfg.Core)
	var outcome corescript.Outcome
	err := t.RunExternalTurn(ctx, externalPrompt+w.Entry, func(ctx context.Context) error {
		var runErr error
		outcome, runErr = script.RunWorkflow(ctx, fx, w)
		return runErr
	})
	return outcome, err
}

func (e extension) MatchExternalTurn(ctx context.Context, turnID string) (bool, error) {
	records, _, err := e.thread.Records()
	if err != nil {
		return false, err
	}
	matched := false
	for _, item := range records {
		if e.vetoed(item, turnID) {
			return false, nil
		}
		if hit, ok := e.prompted(item, turnID); ok {
			matched = hit
		}
	}
	return matched, nil
}

func (e extension) vetoed(item event.Seqd, turnID string) bool {
	value, ok := item.Event.(event.AssistantMessage)
	return ok && value.TurnID == turnID
}

func (e extension) prompted(item event.Seqd, turnID string) (bool, bool) {
	value, ok := item.Event.(event.UserMessage)
	if !ok || value.TurnID != turnID || value.Kind != string(agent.InputPrompt) || !value.Verbatim {
		return false, false
	}
	text, err := e.thread.LoadText(value.Text, value.TextBlob)
	return err == nil && strings.HasPrefix(text, externalPrompt), true
}

func (e extension) ResumeExternalTurn(ctx context.Context) error {
	records, state, err := e.thread.Records()
	if err != nil {
		return err
	}
	if _, ok := state.Scripts[e.thread.ID()]; !ok {
		for _, item := range records {
			if done, ok := item.Event.(event.ScriptCompleted); ok && done.RunID == e.thread.ID() {
				return nil
			}
		}
		return agent.ErrExternalTurnInterrupted
	}
	if nestedErr := e.thread.RecoverNested(ctx, e.thread.ID()+"/"); nestedErr != nil {
		return nestedErr
	}
	_, err = script.RunWorkflow(ctx, e.fx, script.Workflow{ID: e.thread.ID()})
	return err
}
