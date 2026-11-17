package auth

import (
	"maps"
	"slices"
)

func APIKeyProviders() []string {
	return []string{"anthropic", "google", "openai", "openrouter"}
}

func OAuthProviders() []string {
	return []string{"antigravity", "anthropic", "chatgpt", "copilot", "cursor", "devin"}
}

func RunnerServices() []string {
	return []string{"cloudflare", "sandbox0", "sprites"}
}

func Providers() []string {
	ids := make(map[string]struct{}, len(APIKeyProviders())+len(OAuthProviders()))
	for _, id := range slices.Concat(APIKeyProviders(), OAuthProviders()) {
		ids[id] = struct{}{}
	}
	return slices.Sorted(maps.Keys(ids))
}
