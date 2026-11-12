package devin

import (
	"bytes"
	"cmp"
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/devin/wire"
)

const (
	DefaultBaseURL   = "https://server.codeium.com"
	chatPath         = "/exa.api_server_pb.ApiServerService/GetChatMessage"
	modelsPath       = "/exa.api_server_pb.ApiServerService/GetCliModelConfigs"
	jwtPath          = "/exa.auth_pb.AuthService/GetUserJwt"
	defaultMaxTokens = 32000
	jwtMargin        = time.Minute
)

type Config struct {
	BaseURL        string
	APIKey         func(context.Context) (string, error)
	Client         *http.Client
	Retry          model.RetryOptions
	ContextWindows map[string]int
}

type Provider struct {
	cfg     Config
	client  *http.Client
	session session
	outputs sync.Map
}

type session struct {
	turn    chan struct{}
	key     string
	creds   credentials
	expires time.Time
}

func New(cfg Config) *Provider {
	cfg.BaseURL = strings.TrimRight(cmp.Or(cfg.BaseURL, DefaultBaseURL), "/")
	return &Provider{cfg: cfg, client: model.NewHTTPClient(cfg.Client), session: session{turn: make(chan struct{}, 1)}}
}

func APIKeyFromEnv(ctx context.Context) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	key := os.Getenv("DEVIN_API_KEY")
	if key == "" {
		return "", errors.New("DEVIN_API_KEY is not set")
	}
	return key, nil
}

func (p *Provider) key(ctx context.Context) (string, error) {
	if p.cfg.APIKey != nil {
		return p.cfg.APIKey(ctx)
	}
	return APIKeyFromEnv(ctx)
}

func (p *Provider) Stream(ctx context.Context, req model.Request) (model.Stream, error) {
	cascade, err := randomID()
	if err != nil {
		return nil, err
	}
	stream := model.NewEventStream(ctx, 64)
	go p.run(stream.Context(ctx), stream, req, cascade)
	return stream, nil
}

func (p *Provider) run(ctx context.Context, stream *model.EventStream, req model.Request, cascade string) {
	result := model.RunRetries(ctx, p.cfg.Retry, func(ctx context.Context) model.AttemptResult {
		return p.attempt(ctx, stream, req, cascade)
	})
	if result.Err != nil {
		stream.Emit(model.Error{Err: result.Err, Message: result.Err.Error(), Status: result.Status, Body: result.Body})
	}
	stream.Finish(nil)
}

func (p *Provider) attempt(ctx context.Context, stream *model.EventStream, req model.Request, cascade string) model.AttemptResult {
	key, err := p.key(ctx)
	if err != nil {
		return model.AttemptResult{Status: http.StatusUnauthorized, Err: err}
	}
	creds, cached, err := p.credentials(ctx, key)
	if err != nil {
		return model.AttemptResult{Status: unaryStatus(err), Err: err}
	}
	result := p.chat(ctx, stream, req, chatInput{credentials: creds, cascade: cascade})
	if result.Status != http.StatusUnauthorized || result.SawTool || !cached {
		return result
	}
	if forgetErr := p.forget(ctx, key); forgetErr != nil {
		return model.AttemptResult{Err: forgetErr}
	}
	creds, _, err = p.credentials(ctx, key)
	if err != nil {
		return model.AttemptResult{Status: unaryStatus(err), Err: err}
	}
	return p.chat(ctx, stream, req, chatInput{credentials: creds, cascade: cascade})
}

func (p *Provider) chat(ctx context.Context, stream *model.EventStream, req model.Request, input chatInput) model.AttemptResult {
	execution, err := randomID()
	if err != nil {
		return model.AttemptResult{Err: err}
	}
	input.execution = execution
	input.maxTokens = p.maxTokens(req.Model)
	body, err := encodeChat(req, input)
	if err != nil {
		return model.AttemptResult{Err: err}
	}
	frame, err := gzipFrame(body)
	if err != nil {
		return model.AttemptResult{Err: fmt.Errorf("devin: %w", err)}
	}
	gateCtx, gate := model.NewStartGate(ctx, p.cfg.Retry.StartTimeout)
	defer gate.Close()
	httpReq, err := p.newRequest(gateCtx, chatPath, frame, "application/connect+proto")
	if err != nil {
		return model.AttemptResult{Err: err}
	}
	httpReq.Header.Set("Connect-Content-Encoding", "gzip")
	httpReq.Header.Set("Connect-Accept-Encoding", "gzip")
	httpReq.Header.Set("Accept-Encoding", "identity")
	resp, err := p.client.Do(httpReq)
	if err != nil {
		return model.AttemptResult{Err: gate.Error(gateCtx, fmt.Errorf("devin: request: %w", err))}
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return httpFailure(resp)
	}
	state := newChatState(stream, req.Tools)
	return state.consume(gateCtx, gate, watchIdle(resp.Body, p.cfg.Retry.IdleTimeout))
}

func (p *Provider) maxTokens(id string) int {
	if value, ok := p.outputs.Load(id); ok {
		return value.(int)
	}
	return defaultMaxTokens
}

func (p *Provider) newRequest(ctx context.Context, path string, body []byte, contentType string) (*http.Request, error) {
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, p.cfg.BaseURL+path, bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("devin: create request: %w", err)
	}
	httpReq.Header.Set("Content-Type", contentType)
	httpReq.Header.Set("Connect-Protocol-Version", "1")
	httpReq.Header.Set("User-Agent", connectUserAgent)
	return httpReq, nil
}

func httpFailure(resp *http.Response) model.AttemptResult {
	body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return model.AttemptResult{Status: resp.StatusCode, Err: fmt.Errorf("devin: read error response: %w", err)}
	}
	return model.AttemptResult{
		Status:     resp.StatusCode,
		RetryAfter: model.RetryAfter(resp.Header.Get("Retry-After")),
		Body:       body,
		Err:        fmt.Errorf("devin: http %d: %s", resp.StatusCode, bytes.TrimSpace(body)),
	}
}

type unaryError struct {
	status int
	body   []byte
}

func (e *unaryError) Error() string {
	return fmt.Sprintf("http %d: %s", e.status, bytes.TrimSpace(e.body))
}

func (p *Provider) unary(ctx context.Context, path string, body []byte) ([]byte, error) {
	httpReq, err := p.newRequest(ctx, path, body, "application/proto")
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Accept", "*/*")
	resp, err := p.client.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("devin: %s: %w", path, err)
	}
	defer resp.Body.Close()
	payload, err := io.ReadAll(io.LimitReader(resp.Body, maxFramePayload+1))
	if err != nil {
		return nil, fmt.Errorf("devin: read %s: %w", path, err)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("devin: %s: %w", path, &unaryError{status: resp.StatusCode, body: payload})
	}
	if len(payload) > maxFramePayload {
		return nil, fmt.Errorf("devin: %s response exceeds 16 MiB", path)
	}
	return payload, nil
}

func unaryStatus(err error) int {
	if failure, ok := errors.AsType[*unaryError](err); ok {
		return failure.status
	}
	return 0
}

func (p *Provider) credentials(ctx context.Context, key string) (credentials, bool, error) {
	select {
	case p.session.turn <- struct{}{}:
	case <-ctx.Done():
		return credentials{}, false, fmt.Errorf("devin: session: %w", ctx.Err())
	}
	defer func() { <-p.session.turn }()
	if p.session.key == key && p.session.creds.jwt != "" && (p.session.expires.IsZero() || time.Until(p.session.expires) > jwtMargin) {
		return p.session.creds, true, nil
	}
	creds, err := p.exchange(ctx, key)
	if err != nil {
		return credentials{}, false, err
	}
	p.session.key, p.session.creds, p.session.expires = key, creds, jwtExpiry(creds.jwt)
	return creds, false, nil
}

func (p *Provider) forget(ctx context.Context, key string) error {
	select {
	case p.session.turn <- struct{}{}:
	case <-ctx.Done():
		return fmt.Errorf("devin: session: %w", ctx.Err())
	}
	defer func() { <-p.session.turn }()
	if p.session.key == key {
		p.session.creds = credentials{}
	}
	return nil
}

func (p *Provider) exchange(ctx context.Context, key string) (credentials, error) {
	wireKey := sessionToken(key)
	jwt, err := p.requestJWT(ctx, wireKey)
	if unaryStatus(err) == http.StatusUnauthorized {
		wireKey = key
		jwt, err = p.requestJWT(ctx, wireKey)
	}
	if err != nil {
		return credentials{}, err
	}
	return credentials{wireKey: wireKey, jwt: jwt}, nil
}

func (p *Provider) requestJWT(ctx context.Context, wireKey string) (string, error) {
	body := wire.AppendGetUserJWTRequest(nil, &wire.GetUserJWTRequest{Metadata: metadata(wireKey, "")})
	payload, err := p.unary(ctx, jwtPath, body)
	if err != nil {
		return "", err
	}
	var response wire.GetUserJWTResponse
	if err := wire.ConsumeGetUserJWTResponse(payload, &response); err != nil {
		return "", fmt.Errorf("devin: decode user jwt: %w", err)
	}
	if len(response.UserJWTView) == 0 {
		return "", errors.New("devin: GetUserJwt returned no token")
	}
	return string(response.UserJWTView), nil
}

func jwtExpiry(token string) time.Time {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return time.Time{}
	}
	claims, err := base64.RawURLEncoding.DecodeString(strings.TrimRight(parts[1], "="))
	if err != nil {
		return time.Time{}
	}
	decoder := jsontext.NewDecoder(bytes.NewReader(claims))
	var expires int
	err = model.ReadObjectFields(decoder, func(key string) error {
		if key != "exp" {
			return decoder.SkipValue()
		}
		var readErr error
		expires, readErr = model.ReadInt(decoder)
		return readErr
	})
	if err != nil || expires <= 0 {
		return time.Time{}
	}
	return time.Unix(int64(expires), 0)
}

func randomID() (string, error) {
	var raw [16]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("devin: random id: %w", err)
	}
	hexed := hex.EncodeToString(raw[:])
	return hexed[0:8] + "-" + hexed[8:12] + "-" + hexed[12:16] + "-" + hexed[16:20] + "-" + hexed[20:32], nil
}

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	key, err := p.key(ctx)
	if err != nil {
		return nil, err
	}
	body := wire.AppendModelConfigRequest(nil, &wire.GetCliModelConfigsRequest{Metadata: discoveryMetadata(key)})
	payload, err := p.unary(ctx, modelsPath, body)
	if err != nil {
		return nil, err
	}
	var response wire.GetCliModelConfigsResponse
	if err := wire.ConsumeModelConfigResponse(payload, &response); err != nil {
		return nil, fmt.Errorf("devin: decode model configs: %w", err)
	}
	models := make([]model.Model, 0, len(response.Models))
	seen := make(map[string]bool, len(response.Models))
	for index := range response.Models {
		item, ok := p.listedModel(&response.Models[index], seen)
		if ok {
			models = append(models, item)
		}
	}
	return models, nil
}

const displayModelRouter = 3

func (p *Provider) listedModel(config *wire.ClientModelConfig, seen map[string]bool) (model.Model, bool) {
	id := strings.TrimSpace(string(config.ModelUIDView))
	if config.Disabled || id == "" || seen[id] {
		return model.Model{}, false
	}
	info := config.ModelInfo
	if info == nil {
		info = &wire.ModelInfo{}
	}
	if info.ModelRouter || info.DisplayOption == displayModelRouter {
		return model.Model{}, false
	}
	seen[id] = true
	reported := int(cmp.Or(config.ContextWindow, info.MaxTokens))
	if info.MaxOutputTokens > 0 {
		p.outputs.Store(id, int(info.MaxOutputTokens))
	}
	return model.Model{
		ID:         id,
		Context:    model.ContextWindow(id, reported, p.cfg.ContextWindows),
		MaxOutput:  int(info.MaxOutputTokens),
		Images:     config.SupportsImages,
		ToolChoice: true,
	}, true
}
