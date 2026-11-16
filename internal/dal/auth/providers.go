package auth

import (
	"maps"
	"slices"
)

var APIKeyProviders = []string{"anthropic", "google", "openai", "openrouter"}

var OAuthProviders = []string{"antigravity", "anthropic", "chatgpt", "copilot", "cursor", "devin"}

var RunnerServices = []string{"cloudflare", "sandbox0", "sprites"}

func Providers() []string {
	ids := make(map[string]struct{}, len(APIKeyProviders)+len(OAuthProviders))
	for _, id := range slices.Concat(APIKeyProviders, OAuthProviders) {
		ids[id] = struct{}{}
	}
	return slices.Sorted(maps.Keys(ids))
}
