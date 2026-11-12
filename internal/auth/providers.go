package auth

import (
	"slices"
)

var APIKeyProviders = []string{"anthropic", "google", "openai", "openrouter"}

var OAuthProviders = []string{"antigravity", "chatgpt", "cursor", "devin"}

var RunnerServices = []string{"cloudflare", "sandbox0", "sprites"}

func Providers() []string {
	return slices.Sorted(slices.Values(slices.Concat(APIKeyProviders, OAuthProviders)))
}
