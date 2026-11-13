package chatgpt

import (
	"reflect"
	"testing"

	"github.com/gosuda/dalgona/internal/model"
)

func TestStaticModelFlagReachesModel(t *testing.T) {
	saved := staticModels
	t.Cleanup(func() { staticModels = saved })
	staticModels = []staticModel{{id: "text-only", context: 1000}, {id: "vision", context: 1000, images: true}}
	models, err := New(Config{}).Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	want := []model.Model{
		{ID: "text-only", Context: 1000, ToolChoice: true},
		{ID: "vision", Context: 1000, ToolChoice: true, Images: true},
	}
	if !reflect.DeepEqual(models, want) {
		t.Fatalf("models = %+v, want %+v", models, want)
	}
}
