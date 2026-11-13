package rpc

import (
	"reflect"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

type listedModel struct {
	ID            string   `json:"id"`
	Provider      string   `json:"provider"`
	ContextWindow int      `json:"contextWindow"`
	Presets       []string `json:"presets"`
}

func listedModelByID(t *testing.T, models []listedModel, id string) listedModel {
	t.Helper()
	for _, item := range models {
		if item.ID == id {
			return item
		}
	}
	t.Fatalf("model/list has no %s: %+v", id, models)
	return listedModel{}
}

func TestModelListCarriesContextAndPresets(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	recordRequestsListing(t, r, `{"data":[{"id":"gpt-6.1-sol"},{"id":"gpt-test"}]}`)
	r.cfg.Config.Models = map[string]config.ModelSpec{"openai/gpt-test": {ContextWindow: 50000}}
	reopen(t, r)
	c := r.connect()
	var response struct {
		Data []listedModel `json:"data"`
	}
	c.ok("model/list", map[string]any{}, &response)
	sol := listedModelByID(t, response.Data, "openai/gpt-6.1-sol")
	want := listedModel{ID: "openai/gpt-6.1-sol", Provider: "openai", ContextWindow: 128000, Presets: []string{"default"}}
	if !reflect.DeepEqual(sol, want) {
		t.Fatalf("openai/gpt-6.1-sol = %+v, want %+v", sol, want)
	}
	test := listedModelByID(t, response.Data, "openai/gpt-test")
	if test.ContextWindow != 50000 || len(test.Presets) != 0 {
		t.Fatalf("openai/gpt-test = %+v, want context 50000 and no presets", test)
	}
}
