package chatcompat

import (
	"bytes"
	"cmp"
	"context"
	"encoding/base64"
	"fmt"
	"io"
	"net/http"
	"os"
	"slices"
	"strings"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/model"
)

type Config struct {
	BaseURL        string
	APIKey         func(context.Context) (string, error)
	APIKeyEnv      string
	Client         *http.Client
	Retry          model.RetryOptions
	ContextWindows map[string]int
}

type Provider struct {
	cfg    Config
	client *http.Client
}

func New(cfg Config) *Provider {
	if cfg.BaseURL == "" {
		cfg.BaseURL = "https://api.openai.com/v1"
	}
	return &Provider{cfg: cfg, client: model.NewHTTPClient(cfg.Client)}
}

func APIKeyFromEnv(name string) func(context.Context) (string, error) {
	if name == "" {
		name = "OPENAI_API_KEY"
	}
	return func(ctx context.Context) (string, error) {
		if err := ctx.Err(); err != nil {
			return "", err
		}
		key := os.Getenv(name)
		if key == "" {
			return "", fmt.Errorf("%s is not set", name)
		}
		return key, nil
	}
}

func (p *Provider) Stream(ctx context.Context, req model.Request) (model.Stream, error) {
	stream := model.NewEventStream(ctx, 64)
	go p.run(stream.Context(ctx), stream, req)
	return stream, nil
}

func (p *Provider) run(ctx context.Context, stream *model.EventStream, req model.Request) {
	result := model.RunRetries(ctx, p.cfg.Retry, func(ctx context.Context) model.AttemptResult {
		return p.attempt(ctx, stream, req)
	})
	if result.Err != nil {
		stream.Emit(model.Error{Err: result.Err, Message: result.Err.Error(), Status: result.Status, Body: result.Body})
	}
	stream.Finish(nil)
}

func (p *Provider) attempt(ctx context.Context, stream *model.EventStream, req model.Request) model.AttemptResult {
	key, err := p.key(ctx)
	if err != nil {
		return model.AttemptResult{Status: http.StatusUnauthorized, Err: err}
	}
	body, err := encodeRequest(req)
	if err != nil {
		return model.AttemptResult{Err: err}
	}
	endpoint := strings.TrimRight(p.cfg.BaseURL, "/") + "/chat/completions"
	gateCtx, gate := model.NewStartGate(ctx, p.cfg.Retry.StartTimeout)
	defer gate.Close()
	httpReq, err := http.NewRequestWithContext(gateCtx, http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return model.AttemptResult{Err: fmt.Errorf("chatcompat: create request: %w", err)}
	}
	httpReq.Header.Set("Authorization", "Bearer "+key)
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Accept", "text/event-stream")
	resp, err := p.client.Do(httpReq)
	if err != nil {
		return model.AttemptResult{Err: gate.Error(gateCtx, fmt.Errorf("chatcompat: request: %w", err))}
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return httpFailure(resp)
	}
	state := responseState{stream: stream, freeform: model.FreeformToolNames(req.Tools)}
	return state.consume(gateCtx, gate, resp.Body, p.cfg.Retry)
}

func httpFailure(resp *http.Response) model.AttemptResult {
	body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return model.AttemptResult{Status: resp.StatusCode, Err: fmt.Errorf("chatcompat: read error response: %w", err)}
	}
	return model.AttemptResult{
		Status:     resp.StatusCode,
		RetryAfter: model.RetryAfter(resp.Header.Get("Retry-After")),
		Body:       body,
		Err:        fmt.Errorf("chatcompat: http %d: %s", resp.StatusCode, bytes.TrimSpace(body)),
	}
}

func (p *Provider) key(ctx context.Context) (string, error) {
	if p.cfg.APIKey != nil {
		return p.cfg.APIKey(ctx)
	}
	return APIKeyFromEnv(p.cfg.APIKeyEnv)(ctx)
}

func encodeRequest(req model.Request) ([]byte, error) {
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	encoder.BeginObject()
	encoder.StringKey("model", req.Model)
	encoder.Key("messages")
	encoder.BeginArray()
	if req.System != "" {
		encodeTextMessage(encoder, "system", req.System)
	}
	for _, message := range req.Messages {
		encodeMessage(encoder, message)
	}
	encoder.EndArray()
	encodeTools(encoder, req.Tools)
	encoder.BoolKey("stream", true)
	encoder.Key("stream_options")
	encoder.BeginObject()
	encoder.BoolKey("include_usage", true)
	encoder.EndObject()
	if req.Reasoning != "" && req.Reasoning != "default" {
		encoder.StringKey("reasoning_effort", req.Reasoning)
	}
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return nil, fmt.Errorf("chatcompat: encode request: %w", err)
	}
	return buffer.Bytes(), nil
}

func encodeTextMessage(encoder *model.JSONEncoder, role, text string) {
	encoder.BeginObject()
	encoder.StringKey("role", role)
	encoder.StringKey("content", text)
	encoder.EndObject()
}

func encodeMessage(encoder *model.JSONEncoder, message model.Message) {
	if message.Role != "user" || len(message.ToolResults) == 0 {
		encodeAssistantOrUser(encoder, message)
	}
	for _, result := range message.ToolResults {
		encoder.BeginObject()
		encoder.StringKey("role", "tool")
		encoder.StringKey("tool_call_id", result.ID)
		encoder.StringKey("content", model.ToolResultText(result.Output))
		encoder.EndObject()
	}
	if message.Role == "user" && len(message.ToolResults) != 0 && (message.Text != "" || len(message.Images) != 0) {
		encoder.BeginObject()
		encoder.StringKey("role", "user")
		encodeUserContent(encoder, message)
		encoder.EndObject()
	}
}

func encodeAssistantOrUser(encoder *model.JSONEncoder, message model.Message) {
	encoder.BeginObject()
	encoder.StringKey("role", message.Role)
	switch {
	case message.Text == "" && message.Role == "assistant":
		encoder.Key("content")
		encoder.Null()
	case message.Role == "user":
		encodeUserContent(encoder, message)
	default:
		encoder.StringKey("content", message.Text)
	}
	if message.Reasoning != "" {
		encoder.StringKey("reasoning_content", message.Reasoning)
	}
	encodeMessageCalls(encoder, message.ToolCalls)
	encoder.EndObject()
}

func encodeUserContent(encoder *model.JSONEncoder, message model.Message) {
	if len(message.Images) == 0 {
		encoder.StringKey("content", message.Text)
		return
	}
	encoder.Key("content")
	encoder.BeginArray()
	for _, image := range message.Images {
		encodeImagePart(encoder, image)
	}
	if message.Text != "" {
		encodeTextPart(encoder, message.Text)
	}
	encoder.EndArray()
}

func encodeImagePart(encoder *model.JSONEncoder, image model.Image) {
	encoder.BeginObject()
	encoder.StringKey("type", "image_url")
	encoder.Key("image_url")
	encoder.BeginObject()
	encoder.StringKey("url", "data:"+image.Mime+";base64,"+base64.StdEncoding.EncodeToString(image.Data))
	encoder.EndObject()
	encoder.EndObject()
}

func encodeTextPart(encoder *model.JSONEncoder, text string) {
	encoder.BeginObject()
	encoder.StringKey("type", "text")
	encoder.StringKey("text", text)
	encoder.EndObject()
}

func encodeMessageCalls(encoder *model.JSONEncoder, calls []model.ToolCall) {
	if len(calls) == 0 {
		return
	}
	encoder.Key("tool_calls")
	encoder.BeginArray()
	for _, call := range calls {
		encoder.BeginObject()
		encoder.StringKey("id", call.ID)
		encoder.StringKey("type", "function")
		encoder.Key("function")
		encoder.BeginObject()
		encoder.StringKey("name", call.Name)
		encoder.StringKey("arguments", callArguments(call))
		encoder.EndObject()
		encoder.EndObject()
	}
	encoder.EndArray()
}

func callArguments(call model.ToolCall) string {
	if !call.Freeform {
		return string(call.Args)
	}
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	encoder.BeginObject()
	encoder.StringKey("input", string(call.Args))
	encoder.EndObject()
	return strings.TrimSuffix(buffer.String(), "\n")
}

func encodeTools(encoder *model.JSONEncoder, tools []model.Tool) {
	if len(tools) == 0 {
		return
	}
	encoder.Key("tools")
	encoder.BeginArray()
	for _, tool := range tools {
		encoder.BeginObject()
		encoder.StringKey("type", "function")
		encoder.Key("function")
		encoder.BeginObject()
		encoder.StringKey("name", tool.Name)
		encoder.BoolKey("strict", false)
		if tool.Description != "" {
			encoder.StringKey("description", tool.Description)
		}
		if tool.Freeform {
			encoder.Key("parameters")
			encoder.FreeformSchema()
		} else {
			encoder.ValueKey("parameters", tool.JSONSchema())
		}
		encoder.EndObject()
		encoder.EndObject()
	}
	encoder.EndArray()
}

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	key, err := p.key(ctx)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, strings.TrimRight(p.cfg.BaseURL, "/")+"/models", nil)
	if err != nil {
		return nil, fmt.Errorf("chatcompat: create models request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+key)
	resp, err := p.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("chatcompat: models request: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, readErr := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		if readErr != nil {
			return nil, fmt.Errorf("chatcompat: read models error response: %w", readErr)
		}
		return nil, fmt.Errorf("chatcompat: models http %d: %s", resp.StatusCode, bytes.TrimSpace(body))
	}
	return decodeModels(resp.Body, p.cfg.ContextWindows)
}

// source: .references/pi/packages/ai/src/providers/openrouter-api.ts
type modelList struct {
	Data []struct {
		ID          string `json:"id"`
		Context     int    `json:"context_length"`
		TopProvider struct {
			Context   int `json:"context_length"`
			MaxOutput int `json:"max_completion_tokens"`
		} `json:"top_provider"`
		Architecture struct {
			InputModalities []string `json:"input_modalities"`
		} `json:"architecture"`
	} `json:"data"`
}

func decodeModels(reader io.Reader, windows map[string]int) ([]model.Model, error) {
	var payload modelList
	if err := json.UnmarshalRead(reader, &payload); err != nil {
		return nil, fmt.Errorf("chatcompat: decode models: %w", err)
	}
	models := make([]model.Model, 0, len(payload.Data))
	for _, item := range payload.Data {
		reported := cmp.Or(item.Context, item.TopProvider.Context)
		models = append(models, model.Model{
			ID:         item.ID,
			Context:    model.ContextWindow(item.ID, reported, windows),
			MaxOutput:  item.TopProvider.MaxOutput,
			ToolChoice: true,
			Images:     acceptsImages(item.Architecture.InputModalities),
		})
	}
	return models, nil
}

func acceptsImages(modalities []string) bool {
	return slices.Contains(modalities, "image")
}
