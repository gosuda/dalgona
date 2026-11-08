package model

import (
	"fmt"
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

func ResolveRole(role, threadModel string, offered []string) (string, string) {
	if role == "" || role == "default" {
		return threadModel, ""
	}
	if strings.Contains(role, "/") {
		return role, ""
	}
	provider, _, err := ParseModelID(threadModel)
	if err != nil || (role != "smol" && role != "slow") {
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
	if role == "slow" {
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
