package chatgpt

import (
	"net/http"
	"testing"

	"github.com/gosuda/dalgona/internal/model"
)

func outageModels(t *testing.T) []model.Model {
	t.Helper()
	models := okModels()
	models.update(func(b *backend) { b.status = http.StatusServiceUnavailable })
	s := newSite(t, okRegistry("0.153.0"), models)
	account := "acct-A"
	got, err := New(s.config(t.TempDir(), &account)).Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return got
}

func TestStaticModelsAcceptImages(t *testing.T) {
	models := outageModels(t)
	if len(models) != 9 {
		t.Fatalf("models = %d, want 9", len(models))
	}
	for _, item := range models {
		if !item.Images {
			t.Fatalf("%s does not report image input", item.ID)
		}
	}
	saved := staticModels
	t.Cleanup(func() { staticModels = saved })
	staticModels = []staticModel{{id: "text-only", context: 272000, efforts: legacyEffort, images: false}}
	models = outageModels(t)
	if len(models) != 1 || models[0].ID != "text-only" || models[0].Images {
		t.Fatalf("models = %+v, want one entry without image input", models)
	}
}
