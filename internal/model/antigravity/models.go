package antigravity

import (
	"context"
	"fmt"

	"github.com/gosuda/dalgona/internal/model"
)

type staticModel struct {
	id        string
	wire      string
	context   int
	maxOutput int
	efforts   []string
	routes    map[string]string
}

var (
	levelEfforts = []string{"minimal", "low", "medium", "high"}
	tierEfforts  = []string{"low", "medium", "high"}
)

func flashRoutes(prefix string) map[string]string {
	return map[string]string{"minimal": prefix + "-low", "low": prefix + "-low", "medium": prefix + "-medium", "high": prefix + "-high"}
}

var staticModels = []staticModel{
	{id: "gemini-4-argon", wire: "gemini-4-argon-low", context: 1048576, maxOutput: 65536, efforts: tierEfforts, routes: map[string]string{"low": "gemini-4-argon-low", "medium": "gemini-4-argon-medium", "high": "gemini-4-argon-high"}},
	{id: "gemini-3.8-flash", wire: "gemini-3.8-flash-low", context: 1048576, maxOutput: 65536, efforts: levelEfforts, routes: flashRoutes("gemini-3.8-flash")},
	{id: "gemini-3.7-flash", wire: "gemini-3.7-flash-low", context: 1048576, maxOutput: 65536, efforts: levelEfforts, routes: flashRoutes("gemini-3.7-flash")},
	{id: "gemini-3.6-flash", wire: "gemini-3.6-flash-low", context: 1048576, maxOutput: 65536, efforts: levelEfforts, routes: flashRoutes("gemini-3.6-flash")},
	{id: "gemini-3-pro", wire: "gemini-3-pro-low", context: 1048576, maxOutput: 65535, efforts: []string{"low", "high"}, routes: map[string]string{"low": "gemini-3-pro-low", "high": "gemini-3-pro-high"}},
	{id: "gemini-3.5-flash-lite", wire: "gemini-3.5-flash-lite", context: 1048576, maxOutput: 65535, efforts: levelEfforts},
	{id: "gemini-3.1-flash-lite", wire: "gemini-3.1-flash-lite", context: 1048576, maxOutput: 65535, efforts: levelEfforts},
}

func wireModel(id, effort string) string {
	for _, item := range staticModels {
		if item.id != id {
			continue
		}
		if routed, ok := item.routes[effort]; ok {
			return routed
		}
		return item.wire
	}
	return id
}

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("antigravity: models: %w", err)
	}
	models := make([]model.Model, len(staticModels))
	for index, item := range staticModels {
		models[index] = model.Model{
			ID:         item.id,
			Context:    model.ContextWindow(item.id, item.context, p.cfg.ContextWindows),
			MaxOutput:  item.maxOutput,
			Efforts:    item.efforts,
			ToolChoice: true,
		}
	}
	return models, nil
}
