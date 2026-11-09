package agent

import (
	"context"
	"errors"
	"fmt"
	"slices"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/prompt"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/keywords"
	"github.com/gosuda/dalgona/internal/model"
)

func (h *Host) Config() config.Config { return h.cfg.Config }

func (h *Host) Auth() *auth.Store { return h.cfg.Auth }

func (h *Host) Keywords() []keywords.Keyword { return slices.Clone(h.keywords) }

func (h *Host) Skills(cwd string) ([]prompt.Skill, error) {
	if h.cfg.Skills == nil {
		return nil, nil
	}
	return h.cfg.Skills(cwd)
}

func (h *Host) Models(ctx context.Context) ([]model.Model, error) {
	registry, err := h.cfg.Providers(nil)
	if err != nil {
		return nil, fmt.Errorf("agent: providers: %w", err)
	}
	var out []model.Model
	var errs []error
	for _, provider := range h.cfg.ProviderIDs {
		adapter, ok := registry.Provider(provider)
		if !ok {
			errs = append(errs, fmt.Errorf("agent: provider %s is not registered", provider))
			continue
		}
		models, err := adapter.Models(ctx)
		if err != nil {
			errs = append(errs, fmt.Errorf("agent: list %s models: %w", provider, err))
			continue
		}
		for _, item := range models {
			capability := h.capabilities(ctx, registry, provider+"/"+item.ID)
			capability.ID = provider + "/" + item.ID
			out = append(out, capability)
		}
	}
	if len(out) == 0 {
		return nil, errors.Join(errs...)
	}
	for _, err := range errs {
		h.log.Warn("agent: model listing skipped a provider", "error", err)
	}
	return out, nil
}

func (h *Host) Blob(ref *event.BlobRef) ([]byte, error) {
	if ref == nil {
		return nil, errors.New("agent: nil blob reference")
	}
	data, err := h.journal.Blob(ref.Blob)
	if err != nil {
		return nil, fmt.Errorf("agent: blob %s: %w", ref.Blob, err)
	}
	return data, nil
}

func (t *Thread) Tree() ([]event.TurnNode, error) {
	return ask(t, func(a *actor) []event.TurnNode { return event.Tree(a.records) })
}

func (t *Thread) SetName(ctx context.Context, name string) error {
	_, err := t.appendEvents(ctx, event.ThreadNamed{Name: name})
	return err
}

func (h *Host) Archive(ctx context.Context, id string) error {
	thread, err := h.Resume(ctx, id)
	if err != nil {
		return err
	}
	return askErr(thread, func(a *actor) error {
		if a.busy || a.state.TurnID != "" {
			return runningError(id)
		}
		_, err := a.append(event.ThreadArchived{Archived: true})
		return err
	})
}

func (h *Host) Unarchive(ctx context.Context, id string) error {
	thread, err := h.Resume(ctx, id)
	if err != nil {
		return err
	}
	_, err = thread.appendEvents(ctx, event.ThreadArchived{Archived: false})
	return err
}

func (h *Host) Archived(id string) bool {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.sessions[id].Archived
}

func (h *Host) RecordRunnerBound(ctx context.Context, threadID string, bound event.RunnerBound) error {
	h.mu.Lock()
	thread, open := h.threads[threadID]
	h.mu.Unlock()
	if !open {
		return fmt.Errorf("agent: thread %s is not open", threadID)
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	return askErr(thread, func(a *actor) error {
		if a.state.Runner != nil && *a.state.Runner == bound {
			return nil
		}
		_, err := a.append(bound)
		return err
	})
}
