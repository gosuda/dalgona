package chatgpt

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"net/http"

	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/openai"
)

// source: .references/pi/packages/ai/src/api/openai-codex-responses.ts
const (
	DefaultBaseURL      = "https://chatgpt.com/backend-api/codex"
	defaultInstructions = "You are a helpful assistant."
)

type Config struct {
	BaseURL        string
	Token          func(context.Context) (oauth.Token, error)
	Client         *http.Client
	Retry          model.RetryOptions
	ContextWindows map[string]int
}

type Provider struct {
	responses      *openai.Provider
	contextWindows map[string]int
}

type staticModel struct {
	id      string
	context int
	efforts []string
}

var (
	fullEfforts  = []string{"low", "medium", "high", "xhigh", "max", "ultra"}
	lunaEfforts  = []string{"low", "medium", "high", "xhigh", "max"}
	legacyEffort = []string{"low", "medium", "high", "xhigh"}
)

// source: .references/codex/codex-rs/models-manager/models.json
var staticModels = []staticModel{
	{id: "gpt-6.1-sol", context: 272000, efforts: fullEfforts},
	{id: "gpt-6-astra", context: 272000, efforts: fullEfforts},
	{id: "gpt-6-sol", context: 272000, efforts: fullEfforts},
	{id: "gpt-6-luna", context: 272000, efforts: lunaEfforts},
	{id: "gpt-5.6-sol", context: 272000, efforts: fullEfforts},
	{id: "gpt-6.1-terra", context: 500000, efforts: fullEfforts},
	{id: "gpt-5.6-terra", context: 272000, efforts: fullEfforts},
	{id: "gpt-5.6-luna", context: 272000, efforts: lunaEfforts},
	{id: "gpt-5.5", context: 272000, efforts: legacyEffort},
}

func New(cfg Config) *Provider {
	client := *model.NewHTTPClient(cfg.Client)
	client.Transport = accountTransport{next: cmp.Or[http.RoundTripper](client.Transport, http.DefaultTransport), token: cfg.Token}
	responses := openai.New(openai.Config{
		BaseURL:        cmp.Or(cfg.BaseURL, DefaultBaseURL),
		APIKey:         accessToken(cfg.Token),
		Client:         &client,
		Retry:          cfg.Retry,
		ContextWindows: cfg.ContextWindows,
	})
	return &Provider{responses: responses, contextWindows: cfg.ContextWindows}
}

func accessToken(token func(context.Context) (oauth.Token, error)) func(context.Context) (string, error) {
	return func(ctx context.Context) (string, error) {
		value, err := loadToken(ctx, token)
		return value.Access, err
	}
}

func loadToken(ctx context.Context, token func(context.Context) (oauth.Token, error)) (oauth.Token, error) {
	if token == nil {
		return oauth.Token{}, errors.New("chatgpt: not logged in; run /login chatgpt")
	}
	value, err := token(ctx)
	if err != nil {
		return oauth.Token{}, fmt.Errorf("chatgpt: token: %w", err)
	}
	if value.Access == "" {
		return oauth.Token{}, errors.New("chatgpt: token has no access token; run /login chatgpt")
	}
	if value.Account == "" {
		return oauth.Token{}, errors.New("chatgpt: token has no account id; run /login chatgpt")
	}
	return value, nil
}

func (p *Provider) Stream(ctx context.Context, req model.Request) (model.Stream, error) {
	if req.System == "" {
		req.System = defaultInstructions
	}
	return p.responses.Stream(ctx, req)
}

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("chatgpt: models: %w", err)
	}
	models := make([]model.Model, len(staticModels))
	for index, item := range staticModels {
		models[index] = model.Model{
			ID:         item.id,
			Context:    model.ContextWindow(item.id, item.context, p.contextWindows),
			Efforts:    item.efforts,
			ToolChoice: true,
		}
	}
	return models, nil
}

type accountTransport struct {
	next  http.RoundTripper
	token func(context.Context) (oauth.Token, error)
}

func (t accountTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	token, err := loadToken(req.Context(), t.token)
	if err != nil {
		closeBody(req)
		return nil, err
	}
	outbound := req.Clone(req.Context())
	outbound.Header.Set("Authorization", "Bearer "+token.Access)
	outbound.Header.Set("chatgpt-account-id", token.Account)
	outbound.Header.Set("OpenAI-Beta", "responses=experimental")
	outbound.Header.Set("originator", "codex_cli_rs")
	response, err := t.next.RoundTrip(outbound)
	if err != nil {
		return nil, fmt.Errorf("chatgpt: round trip: %w", err)
	}
	return response, nil
}

func closeBody(req *http.Request) {
	if req.Body != nil {
		_ = req.Body.Close()
	}
}
