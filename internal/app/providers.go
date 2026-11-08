package app

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"maps"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/anthropic"
	"github.com/gosuda/dalgona/internal/model/antigravity"
	"github.com/gosuda/dalgona/internal/model/chatcompat"
	"github.com/gosuda/dalgona/internal/model/chatgpt"
	"github.com/gosuda/dalgona/internal/model/cursor"
	"github.com/gosuda/dalgona/internal/model/devin"
	"github.com/gosuda/dalgona/internal/model/google"
	"github.com/gosuda/dalgona/internal/model/openai"
)

type credentials struct {
	store   *auth.Store
	product config.Product
	log     *slog.Logger
	mu      sync.Mutex
	sources map[string]*sourceEntry
}

type sourceEntry struct {
	source *oauth.Source
	stored auth.Credential
}

type unlisted struct {
	model.Provider
	id  string
	log *slog.Logger
}

func (u unlisted) Models(context.Context) ([]model.Model, error) {
	u.log.Debug("app: model listing skipped a provider without a credential", "provider", u.id)
	return nil, nil
}

func newCredentials(store *auth.Store, product config.Product, logger *slog.Logger) *credentials {
	return &credentials{
		store: store, product: cmp.Or(product, config.ProductDalgona), log: cmp.Or(logger, slog.Default()),
		sources: make(map[string]*sourceEntry),
	}
}

func (c *credentials) stored(ctx context.Context, provider, env string) (auth.Credential, error) {
	credential, err := c.store.TokenWithEnv(ctx, provider, env)
	if errors.Is(err, auth.ErrNoCredential) {
		return auth.Credential{}, fmt.Errorf("%w; run %s login %s", err, c.product, provider)
	}
	if err != nil {
		return auth.Credential{}, fmt.Errorf("%s credential: %w", provider, err)
	}
	return credential, nil
}

func (c *credentials) usable(provider, env string) bool {
	credential, err := c.store.TokenWithEnv(context.Background(), provider, env)
	return err == nil && cmp.Or(credential.Key, credential.Access, credential.Refresh) != ""
}

func (c *credentials) apiKey(provider, env string) func(context.Context) (string, error) {
	return func(ctx context.Context) (string, error) {
		credential, err := c.stored(ctx, provider, env)
		if err != nil {
			return "", err
		}
		if key := cmp.Or(credential.Key, credential.Access); key != "" {
			return key, nil
		}
		return "", fmt.Errorf("%s credential has no key; run %s login %s", provider, c.product, provider)
	}
}

func (c *credentials) token(ctx context.Context, provider string) (oauth.Token, error) {
	return c.source(ctx, provider).Token(ctx)
}

func (c *credentials) access(provider string) func(context.Context) (string, error) {
	return func(ctx context.Context) (string, error) { return c.source(ctx, provider).Access(ctx) }
}

func (c *credentials) source(ctx context.Context, provider string) *oauth.Source {
	stored, err := c.stored(ctx, provider, "")
	c.mu.Lock()
	defer c.mu.Unlock()
	if entry, ok := c.sources[provider]; ok && err == nil && entry.stored == stored {
		return entry.source
	}
	entry := &sourceEntry{stored: stored}
	definition, _ := oauth.ProviderByID(provider)
	load := func(ctx context.Context) (oauth.Token, error) {
		credential, err := c.stored(ctx, provider, "")
		if err != nil {
			return oauth.Token{}, err
		}
		return tokenFrom(credential)
	}
	save := func(_ context.Context, token oauth.Token) error {
		credential := credentialFrom(token)
		if err := c.store.SetCredential(provider, credential); err != nil {
			return err
		}
		c.mu.Lock()
		entry.stored = credential
		c.mu.Unlock()
		return nil
	}
	entry.source = oauth.NewSource(definition, load, save)
	c.sources[provider] = entry
	return entry.source
}

func (c *credentials) devinKey(ctx context.Context) (string, error) {
	credential, err := c.stored(ctx, "devin", "")
	if err == nil && credential.Type == "api_key" && credential.Key != "" {
		return credential.Key, nil
	}
	return c.source(ctx, "devin").Access(ctx)
}

func tokenFrom(credential auth.Credential) (oauth.Token, error) {
	token := oauth.Token{Access: cmp.Or(credential.Access, credential.Key), Refresh: credential.Refresh, Account: credential.Account}
	if credential.Expires == "" {
		return token, nil
	}
	expires, err := time.Parse(time.RFC3339, credential.Expires)
	if err != nil {
		return oauth.Token{}, fmt.Errorf("credential expiry: %w", err)
	}
	token.Expires = expires
	return token, nil
}

func credentialFrom(token oauth.Token) auth.Credential {
	credential := auth.Credential{Type: "oauth", Access: token.Access, Refresh: token.Refresh, Account: token.Account}
	if !token.Expires.IsZero() {
		credential.Expires = token.Expires.UTC().Format(time.RFC3339)
	}
	return credential
}

func (c *credentials) registry(cfg config.Config, onRetry agent.RetryFunc) (*model.Registry, error) {
	retry := model.DefaultRetryOptions()
	retry.StartTimeout, retry.IdleTimeout, retry.OnRetry = cfg.StreamStartTimeout, cfg.StreamIdleTimeout, onRetry
	windows := contextWindows(cfg.Models)
	providers := map[string]model.Provider{
		"openai":      openai.New(openai.Config{APIKey: c.apiKey("openai", ""), Retry: retry, ContextWindows: windows["openai"]}),
		"anthropic":   anthropic.New(anthropic.Config{APIKey: c.apiKey("anthropic", ""), Retry: retry, ContextWindows: windows["anthropic"]}),
		"google":      google.New(google.Config{APIKey: c.apiKey("google", ""), Retry: retry, ContextWindows: windows["google"]}),
		"chatgpt":     chatgpt.New(chatgpt.Config{Token: func(ctx context.Context) (oauth.Token, error) { return c.token(ctx, "chatgpt") }, Retry: retry, ContextWindows: windows["chatgpt"]}),
		"antigravity": antigravity.New(antigravity.Config{Token: c.access("antigravity"), Retry: retry, ContextWindows: windows["antigravity"]}),
		"cursor":      cursor.New(cursor.Config{Token: c.access("cursor"), Retry: retry, ContextWindows: windows["cursor"]}),
		"devin":       devin.New(devin.Config{APIKey: c.devinKey, Retry: retry, ContextWindows: windows["devin"]}),
	}
	envs := make(map[string]string, len(cfg.Providers))
	for id, spec := range cfg.Providers {
		envs[id] = spec.APIKeyEnv
		providers[id] = chatcompat.New(chatcompat.Config{
			BaseURL: spec.BaseURL, APIKey: c.apiKey(id, spec.APIKeyEnv), APIKeyEnv: spec.APIKeyEnv,
			Retry: retry, ContextWindows: windows[id],
		})
	}
	registry := model.NewRegistry()
	var errs []error
	for _, id := range slices.Sorted(maps.Keys(providers)) {
		provider := providers[id]
		if !c.usable(id, envs[id]) {
			provider = unlisted{Provider: provider, id: id, log: c.log}
		}
		errs = append(errs, registry.Register(id, provider))
	}
	if err := errors.Join(errs...); err != nil {
		return nil, fmt.Errorf("provider registry: %w", err)
	}
	return registry, nil
}

func providerIDs(cfg config.Config) []string {
	ids := []string{"anthropic", "antigravity", "chatgpt", "cursor", "devin", "google", "openai"}
	for id := range cfg.Providers {
		ids = append(ids, strings.ToLower(strings.TrimSpace(id)))
	}
	slices.Sort(ids)
	return slices.Compact(ids)
}

func contextWindows(models map[string]config.ModelSpec) map[string]map[string]int {
	windows := make(map[string]map[string]int)
	for id, spec := range models {
		provider, name, err := model.ParseModelID(id)
		if err != nil || spec.ContextWindow <= 0 {
			continue
		}
		if windows[provider] == nil {
			windows[provider] = make(map[string]int)
		}
		windows[provider][name] = spec.ContextWindow
	}
	return windows
}
