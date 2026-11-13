package rpc

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
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
	if len(list.Data) != len(want) {
		t.Fatalf("model/list returned %d models, want %d", len(list.Data), len(want))
	}
	for _, item := range list.Data {
		if !slices.Equal(item.InputModalities, want[item.ID]) {
			t.Fatalf("%s inputModalities = %v, want %v", item.ID, item.InputModalities, want[item.ID])
		}
	}
}
