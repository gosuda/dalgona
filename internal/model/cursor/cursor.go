package cursor

import (
	"bytes"
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

const (
	DefaultBaseURL  = "https://api2.cursor.sh"
	runPath         = "/agent.v1.AgentService/Run"
	modelsPath      = "/agent.v1.AgentService/GetUsableModels"
	clientVersion   = "cli-2026.07.23-e383d2b"
	heartbeatPeriod = 5 * time.Second
	execBeatPeriod  = 3 * time.Second
)

type Config struct {
	BaseURL        string
	Token          func(context.Context) (string, error)
	Client         *http.Client
	Retry          model.RetryOptions
	ContextWindows map[string]int
}

type StreamOptions struct {
	InBand   func(context.Context, model.ToolCall) (model.ToolResult, error)
	StateDir string
}

type Provider struct {
	cfg    Config
	client *http.Client
}

func New(cfg Config) *Provider {
	cfg.BaseURL = strings.TrimRight(cmp.Or(cfg.BaseURL, DefaultBaseURL), "/")
	return &Provider{cfg: cfg, client: model.NewHTTPClient(cfg.Client)}
}

func (p *Provider) Stream(ctx context.Context, req model.Request) (model.Stream, error) {
	return p.StreamWith(ctx, req, StreamOptions{})
}

func (p *Provider) StreamWith(ctx context.Context, req model.Request, options StreamOptions) (model.Stream, error) {
	conv, err := openConversation(options.StateDir)
	if err != nil {
		return nil, err
	}
	return model.LaunchStream(ctx, p.cfg.Retry, func(attemptCtx context.Context, stream *model.EventStream) model.AttemptResult {
		return p.attempt(attemptCtx, stream, turn{req: req, options: options, conv: conv})
	})
}

type turn struct {
	req     model.Request
	options StreamOptions
	conv    *conversation
}

func (p *Provider) token(ctx context.Context) (string, error) {
	if p.cfg.Token == nil {
		return "", errors.New("cursor: not logged in; run /login cursor")
	}
	token, err := p.cfg.Token(ctx)
	if err != nil {
		return "", fmt.Errorf("cursor: token: %w", err)
	}
	return token, nil
}

func (p *Provider) attempt(ctx context.Context, stream *model.EventStream, current turn) model.AttemptResult {
	token, err := p.token(ctx)
	if err != nil {
		return model.AttemptResult{Status: http.StatusUnauthorized, Err: err}
	}
	payload, err := buildRun(current.req, current.conv)
	if err != nil {
		return model.AttemptResult{Err: err}
	}
	tools, err := toolDefinitions(current.req.Tools)
	if err != nil {
		return model.AttemptResult{Err: err}
	}
	gateCtx, gate := model.NewStartGate(ctx, p.cfg.Retry.StartTimeout)
	defer gate.Close()
	reader, pipe := io.Pipe()
	writer := &frameWriter{pipe: pipe}
	body := requestBody{Reader: io.MultiReader(bytes.NewReader(connectwire.AppendFrame(nil, 0, payload)), reader), pipe: reader}
	httpReq, err := p.newRequest(gateCtx, runPath, body, token)
	if err != nil {
		_ = pipe.Close()
		return model.AttemptResult{Err: err}
	}
	httpReq.Header.Set("Content-Type", "application/connect+proto")
	stopBeats := repeat(gateCtx, heartbeatPeriod, func() error {
		return writer.send(&wire.AgentClientMessage{MessageNumber: wire.ClientHeartbeatNumber, MessageView: []byte{}})
	})
	resp, err := p.client.Do(httpReq)
	if err != nil {
		writer.close()
		stopBeats()
		return model.AttemptResult{Err: gate.Error(gateCtx, fmt.Errorf("cursor: request: %w", err))}
	}
	defer resp.Body.Close()
	defer func() {
		writer.close()
		stopBeats()
	}()
	if resp.StatusCode != http.StatusOK {
		return model.HTTPFailure("cursor", resp)
	}
	run := &session{stream: stream, conv: current.conv, inBand: current.options.InBand, tools: tools, writer: writer}
	return run.consume(gateCtx, gate, model.WatchIdle(resp.Body, p.cfg.Retry.IdleTimeout))
}

func (p *Provider) newRequest(ctx context.Context, path string, body io.Reader, token string) (*http.Request, error) {
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, p.cfg.BaseURL+path, body)
	if err != nil {
		return nil, fmt.Errorf("cursor: create request: %w", err)
	}
	requestID, err := randomUUID()
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Connect-Protocol-Version", "1")
	httpReq.Header.Set("Authorization", "Bearer "+token)
	httpReq.Header.Set("X-Ghost-Mode", "true")
	httpReq.Header.Set("X-Cursor-Client-Version", clientVersion)
	httpReq.Header.Set("X-Cursor-Client-Type", "cli")
	httpReq.Header.Set("X-Request-Id", requestID)
	return httpReq, nil
}

type requestBody struct {
	io.Reader
	pipe *io.PipeReader
}

func (b requestBody) Close() error {
	return b.pipe.Close()
}

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	token, err := p.token(ctx)
	if err != nil {
		return nil, err
	}
	body := wire.AppendGetUsableModelsRequest(nil, &wire.GetUsableModelsRequest{})
	httpReq, err := p.newRequest(ctx, modelsPath, bytes.NewReader(body), token)
	if err != nil {
		return nil, err
	}
	httpReq.Header.Set("Content-Type", "application/proto")
	resp, err := p.client.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("cursor: models request: %w", err)
	}
	defer resp.Body.Close()
	payload, err := io.ReadAll(io.LimitReader(resp.Body, maxFramePayload+1))
	if err != nil {
		return nil, fmt.Errorf("cursor: read models: %w", err)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("cursor: models http %d: %s", resp.StatusCode, bytes.TrimSpace(payload))
	}
	var response wire.GetUsableModelsResponse
	if err := wire.ConsumeGetUsableModelsResponse(unaryPayload(payload), &response); err != nil {
		return nil, fmt.Errorf("cursor: decode models: %w", err)
	}
	return p.listModels(response.Models), nil
}

func unaryPayload(payload []byte) []byte {
	frame, err := connectwire.ReadFrame(bytes.NewReader(payload), nil, maxFramePayload)
	if err != nil || frame.Flags != 0 {
		return payload
	}
	return frame.Payload
}

func (p *Provider) listModels(details []wire.ModelDetails) []model.Model {
	models := make([]model.Model, 0, len(details))
	seen := make(map[string]bool, len(details))
	for index := range details {
		id := strings.TrimSpace(string(details[index].ModelIDView))
		if id == "" || seen[id] {
			continue
		}
		seen[id] = true
		models = append(models, model.Model{ID: id, Context: model.ContextWindow(id, 0, p.cfg.ContextWindows), ToolChoice: true})
	}
	return models
}
