package rpc

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
)

type listingProvider struct{ models []model.Model }

func (p listingProvider) Stream(context.Context, model.Request) (model.Stream, error) {
	return nil, errors.New("listing provider streams nothing")
}

func (p listingProvider) Models(context.Context) ([]model.Model, error) { return p.models, nil }

func TestModelListCarriesInputModalities(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	registry := model.NewRegistry()
	provider := listingProvider{models: []model.Model{{ID: "text-only"}, {ID: "vision", Images: true}}}
	if err := registry.Register("openai", provider); err != nil {
		t.Fatal(err)
	}
	r.cfg.Providers = func(agent.RetryFunc) (*model.Registry, error) { return registry, nil }
	r.openHost()
	var list struct {
		Data []struct {
			ID              string   `json:"id"`
			InputModalities []string `json:"inputModalities"`
		} `json:"data"`
	}
	r.connect().ok("model/list", map[string]any{}, &list)
	want := map[string][]string{"openai/text-only": {"text"}, "openai/vision": {"text", "image"}}
	seen := make(map[string]bool, len(list.Data))
	for _, item := range list.Data {
		wantModalities, ok := want[item.ID]
		if !ok {
			t.Fatalf("model/list returned unexpected model %q", item.ID)
		}
		if seen[item.ID] {
			t.Fatalf("model/list returned model %q more than once", item.ID)
		}
		seen[item.ID] = true
		if !slices.Equal(item.InputModalities, wantModalities) {
			t.Fatalf("%s inputModalities = %v, want %v", item.ID, item.InputModalities, wantModalities)
		}
	}
	for id := range want {
		if !seen[id] {
			t.Fatalf("model/list is missing model %q", id)
		}
	}
}
