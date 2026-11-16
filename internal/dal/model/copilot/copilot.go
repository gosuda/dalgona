package copilot

import (
	"cmp"
	"context"
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"net/http"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/chatcompat"
)

const (
	DefaultBaseURL = "https://api.githubcopilot.com"

	integrationID  = "vscode-chat"
	intent         = "conversation-panel"
	initiatorUser  = "user"
	initiatorAgent = "agent"
)

type Config struct {
	Token          func(context.Context) (string, error)
	Client         *http.Client
	Retry          model.RetryOptions
	ContextWindows map[string]int
	TokenURL       string
	BaseURL        string
}

type Provider struct {
	chat    *chatcompat.Provider
	tokens  *tokenSource
	client  *http.Client
	baseURL string
	windows map[string]int
}

type initiatorKey struct{}

func New(cfg Config) *Provider {
	exchange := model.NewHTTPClient(cfg.Client)
	api := *exchange
	api.Transport = identityTransport{
		next:   cmp.Or[http.RoundTripper](exchange.Transport, http.DefaultTransport),
		editor: "dalgona/" + config.Version(),
	}
	tokens := newTokenSource(cfg.Token, exchange, cmp.Or(cfg.TokenURL, DefaultExchangeURL))
	baseURL := cmp.Or(cfg.BaseURL, DefaultBaseURL)
	return &Provider{
		chat: chatcompat.New(chatcompat.Config{
			BaseURL:        baseURL,
			APIKey:         tokens.bearer,
			Client:         &api,
			Retry:          cfg.Retry,
			ContextWindows: cfg.ContextWindows,
		}),
		tokens:  tokens,
		client:  &api,
		baseURL: baseURL,
		windows: cfg.ContextWindows,
	}
}

func (p *Provider) Stream(ctx context.Context, req model.Request) (model.Stream, error) {
	return p.chat.Stream(context.WithValue(ctx, initiatorKey{}, initiatorOf(req)), req)
}

func initiatorOf(req model.Request) string {
	if len(req.Messages) != 0 && len(req.Messages[len(req.Messages)-1].ToolResults) != 0 {
		return initiatorAgent
	}
	return initiatorUser
}

type identityTransport struct {
	next   http.RoundTripper
	editor string
}

func (t identityTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	requestID, err := randomUUID()
	if err != nil {
		closeBody(req)
		return nil, err
	}
	initiator, _ := req.Context().Value(initiatorKey{}).(string)
	outbound := req.Clone(req.Context())
	outbound.Header.Set("Editor-Version", t.editor)
	outbound.Header.Set("Editor-Plugin-Version", t.editor)
	outbound.Header.Set("Copilot-Integration-Id", integrationID)
	outbound.Header.Set("Openai-Intent", intent)
	outbound.Header.Set("X-Request-Id", requestID)
	outbound.Header.Set("X-Initiator", cmp.Or(initiator, initiatorUser))
	response, err := t.next.RoundTrip(outbound)
	if err != nil {
		return nil, fmt.Errorf("copilot: round trip: %w", err)
	}
	return response, nil
}

func closeBody(req *http.Request) {
	if req.Body != nil {
		_ = req.Body.Close()
	}
}

func randomUUID() (string, error) {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("copilot: random id: %w", err)
	}
	raw[6] = raw[6]&0x0f | 0x40
	raw[8] = raw[8]&0x3f | 0x80
	hexed := hex.EncodeToString(raw[:])
	return hexed[0:8] + "-" + hexed[8:12] + "-" + hexed[12:16] + "-" + hexed[16:20] + "-" + hexed[20:32], nil
}
