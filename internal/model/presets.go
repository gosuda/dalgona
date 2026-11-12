package model

import (
	"fmt"
	"slices"
	"strings"
)

type Preset struct {
	Default string
	Smol    string
	Slow    string
}

var Presets = map[string]Preset{
	"openai":    {Default: "gpt-6.1-sol", Smol: "gpt-6-luna", Slow: "gpt-6-astra"},
	"chatgpt":   {Default: "gpt-6.1-sol", Smol: "gpt-6-luna", Slow: "gpt-6-astra"},
	"anthropic": {Default: "claude-opus-5-5", Smol: "claude-haiku-5-5", Slow: "claude-fable-5-1"},
	"google":    {Default: "gemini-3.8-flash", Smol: "gemini-3.5-flash-lite", Slow: "gemini-4-argon"},
}

func PresetFor(provider string) (Preset, bool) {
	preset, ok := Presets[provider]
	return preset, ok
}

var reasoningLowToMax = []string{"low", "medium", "high", "xhigh", "max"}

var presetCapabilities = map[string]Model{
	"openai/gpt-6.1-sol":           {ID: "gpt-6.1-sol", Efforts: reasoningLowToMax},
	"openai/gpt-6-luna":            {ID: "gpt-6-luna"},
	"openai/gpt-6-astra":           {ID: "gpt-6-astra", Efforts: reasoningLowToMax},
	"chatgpt/gpt-6.1-sol":          {ID: "gpt-6.1-sol", Efforts: reasoningLowToMax},
	"chatgpt/gpt-6-luna":           {ID: "gpt-6-luna"},
	"chatgpt/gpt-6-astra":          {ID: "gpt-6-astra", Efforts: reasoningLowToMax},
	"anthropic/claude-opus-5-5":    {ID: "claude-opus-5-5", Context: 1_000_000},
	"anthropic/claude-haiku-5-5":   {ID: "claude-haiku-5-5"},
	"anthropic/claude-fable-5-1":   {ID: "claude-fable-5-1"},
	"google/gemini-3.8-flash":      {ID: "gemini-3.8-flash"},
	"google/gemini-3.5-flash-lite": {ID: "gemini-3.5-flash-lite"},
	"google/gemini-4-argon":        {ID: "gemini-4-argon", Context: 1_048_576},
}

func Capabilities(provider, modelID string) (Model, bool) {
	capability, ok := presetCapabilities[provider+"/"+modelID]
	capability.Efforts = slices.Clone(capability.Efforts)
	return capability, ok
}

func ResolveRole(role, threadModel string, offered []string) (string, string) {
	if role == "" || role == "default" {
		return threadModel, ""
	}
	if strings.Contains(role, "/") {
		return role, ""
	}
	provider, _, err := ParseModelID(threadModel)
	if err != nil || !slices.Contains([]string{"smol", "slow", "advisor"}, role) {
		return threadModel, ""
	}
	candidate, ok := rolePreset(provider, role, offered)
	if !ok {
		return threadModel, ""
	}
	if offeredModel(candidate, offered) {
		return provider + "/" + candidate, ""
	}
	presetID := provider + "/" + candidate
	return threadModel, fmt.Sprintf("roles.%s preset %s is not offered; using %s", role, presetID, threadModel)
}

func rolePreset(provider, role string, offered []string) (string, bool) {
	preset, ok := Presets[provider]
	if !ok {
		if len(offered) == 0 {
			return "", false
		}
		return bareModelID(offered[0]), true
	}
	if role == "slow" || role == "advisor" {
		return preset.Slow, true
	}
	return preset.Smol, true
}

func bareModelID(id string) string {
	if _, modelID, err := ParseModelID(id); err == nil {
		return modelID
	}
	return id
}

func offeredModel(candidate string, offered []string) bool {
	for _, id := range offered {
		if bareModelID(id) == candidate {
			return true
		}
	}
	return false
}

func ClampEffort(request string, supported []string) string {
	target := effortScore(request)
	if target < 0 || len(supported) == 0 {
		return ""
	}
	if best := effortAtOrBelow(target, supported); best != "" {
		return best
	}
	return lowestEffort(supported)
}

func effortAtOrBelow(target int, supported []string) string {
	best, bestScore := "", -1
	for _, effort := range supported {
		score := effortScore(effort)
		if score >= 0 && score <= target && score > bestScore {
			best, bestScore = effort, score
		}
	}
	return best
}

func lowestEffort(supported []string) string {
	lowest := supported[0]
	lowestScore := effortScore(lowest)
	for _, effort := range supported[1:] {
		score := effortScore(effort)
		if score >= 0 && score < lowestScore {
			lowest, lowestScore = effort, score
		}
	}
	return lowest
}

func effortScore(effort string) int {
	switch effort {
	case "minimal":
		return 0
	case "low":
		return 1
	case "medium":
		return 2
	case "high":
		return 3
	case "xhigh":
		return 4
	case "max":
		return 5
	default:
		return -1
	}
}
