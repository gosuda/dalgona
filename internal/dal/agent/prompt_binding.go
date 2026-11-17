package agent

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/agent/compact"
	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/agent/prompt"
)

func (a *actor) establishBase(ctx context.Context) error {
	if a.state.CurrentBaseGeneration != 0 {
		return nil
	}
	base, err := a.t.host.mask(a.t.system)
	if err != nil {
		return err
	}
	window := a.t.host.promptWindow(pathSettings(a.t.created, a.records).Model)
	if len(base)/4 >= window-compact.Reserve(window) {
		return errors.New("agent: system prompt exceeds context window")
	}
	_, err = a.append(ctx, event.PromptBase{Generation: 1, Content: base})
	return err
}

func (a *actor) bindContext(ctx context.Context, turnID string) (requestContext, error) {
	if err := a.establishBase(ctx); err != nil {
		return requestContext{}, err
	}
	if turnID == "" {
		return a.currentContext(), nil
	}
	bound, ok := a.state.PromptBindings[turnID]
	if !ok {
		bound = event.PromptBound{TurnID: turnID, BaseGeneration: a.state.CurrentBaseGeneration, MemoryGeneration: a.state.CurrentMemoryGeneration}
		if _, err := a.append(ctx, bound); err != nil {
			return requestContext{}, err
		}
	}
	return newRequestContext(a.state, bound, a.head())
}

func (a *actor) acceptMemory(ctx context.Context, read memoryRead) {
	host := a.t.host
	if errors.Is(read.err, errMemoryBusy) {
		return
	}
	defer host.refreshes.serve(a.t.id, read.requested)
	if read.err != nil {
		a.memoryNotice(read.err.Error())
		return
	}
	content, err := host.mask(read.content)
	if err != nil {
		a.memoryNotice(err.Error())
		return
	}
	content = strings.TrimSpace(content)
	suffix := prompt.MemorySuffix(content)
	if !utf8.ValidString(content) || len(suffix) > event.MaxMemoryBytes || (len(suffix)+3)/4 > 15000 {
		a.memoryNotice("memory: source exceeds injection bounds")
		return
	}
	if content == a.currentMemory() {
		return
	}
	generation := a.state.CurrentMemoryGeneration + 1
	if _, err := a.append(ctx, event.MemoryUpdated{Generation: generation, Content: content}); err != nil {
		a.memoryNotice(err.Error())
		return
	}
	a.memoryNotice("cache miss: memory changed")
}

func (a *actor) memoryNotice(text string) {
	a.publish(Update{Kind: UpdateWarning, Thread: a.t.id, Warning: a.t.host.maskedText(text)})
}

func (w *worker) initializeContext(ctx context.Context) error {
	if !w.recovering && !w.continuation && w.t.host.memoryEnabled() && w.t.host.refreshes.due(w.t.id) {
		read := w.readMemory(ctx)
		if err := askErr(ctx, w.t, func(a *actor) error {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			a.acceptMemory(ctx, read)
			return nil
		}); err != nil {
			return err
		}
	}
	type result struct {
		context requestContext
		err     error
	}
	bound, err := ask(ctx, w.t, func(a *actor) result {
		value, failure := a.bindContext(ctx, w.turnID)
		return result{context: value, err: failure}
	})
	if err != nil || bound.err != nil {
		return fmt.Errorf("agent: bind prompt: %w", errors.Join(err, bound.err))
	}
	w.requestContext = bound.context
	return nil
}
