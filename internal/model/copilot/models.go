package copilot

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net/http"
	"slices"
	"strings"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/model"
)

const maxCatalogBytes = 4 << 20

type catalog struct {
	Data []catalogEntry `json:"data"`
}

type catalogEntry struct {
	ID            string `json:"id"`
	PickerEnabled bool   `json:"model_picker_enabled"`
	Capabilities  struct {
		Limits struct {
			MaxContext int `json:"max_context_window_tokens"`
			MaxOutput  int `json:"max_output_tokens"`
		} `json:"limits"`
		Supports struct {
			ReasoningEffort jsontext.Value `json:"reasoning_effort"`
		} `json:"supports"`
	} `json:"capabilities"`
}

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	token, err := p.tokens.bearer(ctx)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, strings.TrimRight(p.baseURL, "/")+"/models", nil)
	if err != nil {
		return nil, fmt.Errorf("copilot: create models request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Accept", "application/json")
	resp, err := p.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("copilot: models request: %w", err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(io.LimitReader(resp.Body, maxCatalogBytes+1))
	if err != nil {
		return nil, fmt.Errorf("copilot: read models response: %w", err)
	}
	if len(body) > maxCatalogBytes {
		return nil, errors.New("copilot: models response exceeds 4 MiB")
	}
	if resp.StatusCode < http.StatusOK || resp.StatusCode >= http.StatusMultipleChoices {
		return nil, fmt.Errorf("copilot: models request returned HTTP %d%s", resp.StatusCode, providerDetail(body, token))
	}
	return decodeModels(body, p.windows)
}

func decodeModels(body []byte, windows map[string]int) ([]model.Model, error) {
	var payload catalog
	if err := json.Unmarshal(body, &payload); err != nil {
		return nil, fmt.Errorf("copilot: decode models: %w", err)
	}
	models := make([]model.Model, 0, len(payload.Data))
	seen := make(map[string]struct{}, len(payload.Data))
	for _, entry := range payload.Data {
		if _, repeated := seen[entry.ID]; repeated || !entry.PickerEnabled {
			continue
		}
		seen[entry.ID] = struct{}{}
		limits := entry.Capabilities.Limits
		models = append(models, model.Model{
			ID:         entry.ID,
			Context:    model.ContextWindow(entry.ID, limits.MaxContext, windows),
			MaxOutput:  limits.MaxOutput,
			Efforts:    efforts(entry.Capabilities.Supports.ReasoningEffort),
			ToolChoice: true,
		})
	}
	return models, nil
}

func efforts(raw jsontext.Value) []string {
	var levels []string
	if err := json.Unmarshal(raw, &levels); err != nil {
		return nil
	}
	var kept []string
	for _, level := range levels {
		level = strings.ToLower(strings.TrimSpace(level))
		if level != "" && !slices.Contains(kept, level) {
			kept = append(kept, level)
		}
	}
	return kept
}
