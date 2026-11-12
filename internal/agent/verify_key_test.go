package agent

import (
	"context"
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/model"
)

type fixedModelLister struct{}

func (fixedModelLister) Stream(context.Context, model.Request) (model.Stream, error) {
	return nil, errors.New("stream is not used")
}

func (fixedModelLister) Models(context.Context) ([]model.Model, error) {
	return []model.Model{{ID: "fixed"}}, nil
}

func TestModelsBuildsCredentialRegistryOnce(t *testing.T) {
	builds := 0
	h := newHarness(t, func(h *harness) {
		h.cfg.ProviderIDs = []string{"a", "b", "c"}
		h.cfg.Providers = func(RetryFunc) (*model.Registry, error) {
			builds++
			registry := model.NewRegistry()
			for _, id := range []string{"a", "b", "c"} {
				if err := registry.Register(id, fixedModelLister{}); err != nil {
					return nil, err
				}
			}
			return registry, nil
		}
	})
	builds = 0
	models, err := h.host.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	if builds != 1 {
		t.Fatalf("Models built the credential registry %d times for 3 providers, want 1", builds)
	}
	if len(models) != 3 {
		t.Fatalf("Models returned %d models, want 3", len(models))
	}
}
