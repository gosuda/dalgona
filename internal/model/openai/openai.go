package openai

import (
	"bytes"
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/model"
)

type Config struct {
	BaseURL        string
	APIKey         func(context.Context) (string, error)
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
		cfg.BaseURL = os.Getenv("OPENAI_BASE_URL")
	}
	if cfg.BaseURL == "" {
		cfg.BaseURL = "https://api.openai.com/v1"
	}
	return &Provider{cfg: cfg, client: model.NewHTTPClient(cfg.Client)}
}

func APIKeyFromEnv(ctx context.Context) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	key := os.Getenv("OPENAI_API_KEY")
	if key == "" {
		return "", errors.New("OPENAI_API_KEY is not set")
	}
	return key, nil
}

func (p *Provider) Stream(ctx context.Context, req model.Request) (model.Stream, error) {
	return model.LaunchStream(ctx, p.cfg.Retry, func(attemptCtx context.Context, stream *model.EventStream) model.AttemptResult {
		return p.attempt(attemptCtx, stream, req)
	})
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
	endpoint := strings.TrimRight(p.cfg.BaseURL, "/") + "/responses"
	gateCtx, gate := model.NewStartGate(ctx, p.cfg.Retry.StartTimeout)
	defer gate.Close()
	httpReq, err := http.NewRequestWithContext(gateCtx, http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return model.AttemptResult{Err: fmt.Errorf("openai: create request: %w", err)}
	}
	httpReq.Header.Set("Authorization", "Bearer "+key)
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Accept", "text/event-stream")
	resp, err := p.client.Do(httpReq)
	if err != nil {
		return model.AttemptResult{Err: gate.Error(gateCtx, fmt.Errorf("openai: request: %w", err))}
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return model.HTTPFailure("openai", resp)
	}
	state := responseState{stream: stream}
	return state.consume(gateCtx, gate, resp.Body, p.cfg.Retry)
}

func (p *Provider) key(ctx context.Context) (string, error) {
	if p.cfg.APIKey != nil {
		return p.cfg.APIKey(ctx)
	}
	return APIKeyFromEnv(ctx)
}

func encodeRequest(req model.Request) ([]byte, error) {
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	encoder.BeginObject()
	encoder.StringKey("model", req.Model)
	if req.System != "" {
		encoder.StringKey("instructions", req.System)
	}
	encoder.Key("input")
	encoder.BeginArray()
	customIDs := customToolCallIDs(req)
	for _, message := range req.Messages {
		encodeMessage(encoder, message, customIDs)
	}
	encoder.EndArray()
	encodeTools(encoder, req.Tools)
	encoder.BoolKey("stream", true)
	encoder.BoolKey("store", false)
	encodeReasoning(encoder, req.Reasoning)
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return nil, fmt.Errorf("openai: encode request: %w", err)
	}
	return buffer.Bytes(), nil
}

func customToolCallIDs(req model.Request) map[string]bool {
	names := model.FreeformToolNames(req.Tools)
	ids := make(map[string]bool)
	for _, message := range req.Messages {
		for _, call := range message.ToolCalls {
			if call.Freeform || names[call.Name] {
				ids[call.ID] = true
			}
		}
	}
	return ids
}

func encodeMessage(encoder *model.JSONEncoder, message model.Message, customIDs map[string]bool) {
	if message.Role == "assistant" {
		encodeAssistant(encoder, message)
	} else if message.Text != "" || len(message.Images) > 0 || len(message.ToolResults) == 0 && message.Role != "" {
		encodeTextMessage(encoder, message.Role, message.Text, "input_text", message.Images)
	}
	for _, result := range message.ToolResults {
		encoder.BeginObject()
		encoder.StringKey("type", toolOutputType(customIDs[result.ID]))
		encoder.StringKey("call_id", result.ID)
		encoder.StringKey("output", model.ToolResultText(result.Output))
		encoder.EndObject()
	}
}

func toolOutputType(custom bool) string {
	if custom {
		return "custom_tool_call_output"
	}
	return "function_call_output"
}

func encodeAssistant(encoder *model.JSONEncoder, message model.Message) {
	if message.Text != "" {
		encoder.BeginObject()
		encoder.StringKey("type", "message")
		encoder.StringKey("role", "assistant")
		encoder.StringKey("status", "completed")
		encoder.Key("content")
		encoder.BeginArray()
		encoder.BeginObject()
		encoder.StringKey("type", "output_text")
		encoder.StringKey("text", message.Text)
		encoder.EndObject()
		encoder.EndArray()
		encoder.EndObject()
	}
	for _, call := range message.ToolCalls {
		encodeAssistantCall(encoder, call)
	}
}

func encodeTextMessage(encoder *model.JSONEncoder, role, text, kind string, images []model.Image) {
	encoder.BeginObject()
	encoder.StringKey("role", role)
	encoder.Key("content")
	encoder.BeginArray()
	for _, image := range images {
		encodeInputImage(encoder, image)
	}
	if text != "" || len(images) == 0 {
		encoder.BeginObject()
		encoder.StringKey("type", kind)
		encoder.StringKey("text", text)
		encoder.EndObject()
	}
	encoder.EndArray()
	encoder.EndObject()
}

func encodeInputImage(encoder *model.JSONEncoder, image model.Image) {
	encoder.BeginObject()
	encoder.StringKey("type", "input_image")
	encoder.StringKey("image_url", "data:"+image.Mime+";base64,"+base64.StdEncoding.EncodeToString(image.Data))
	encoder.StringKey("detail", "auto")
	encoder.EndObject()
}

func encodeAssistantCall(encoder *model.JSONEncoder, call model.ToolCall) {
	encoder.BeginObject()
	if call.Freeform {
		encoder.StringKey("type", "custom_tool_call")
		encoder.StringKey("input", string(call.Args))
	} else {
		encoder.StringKey("type", "function_call")
		encoder.StringKey("arguments", string(call.Args))
	}
	encoder.StringKey("call_id", call.ID)
	encoder.StringKey("name", call.Name)
	encoder.EndObject()
}

func encodeTools(encoder *model.JSONEncoder, tools []model.Tool) {
	if len(tools) == 0 {
		return
	}
	encoder.Key("tools")
	encoder.BeginArray()
	for _, tool := range tools {
		encoder.BeginObject()
		if tool.Freeform {
			encoder.StringKey("type", "custom")
		} else {
			encoder.StringKey("type", "function")
			encoder.BoolKey("strict", false)
			encoder.ValueKey("parameters", tool.JSONSchema())
		}
		encoder.StringKey("name", tool.Name)
		if tool.Description != "" {
			encoder.StringKey("description", tool.Description)
		}
		encoder.EndObject()
	}
	encoder.EndArray()
}

func encodeReasoning(encoder *model.JSONEncoder, effort string) {
	if effort == "" || effort == "default" {
		return
	}
	encoder.Key("reasoning")
	encoder.BeginObject()
	encoder.StringKey("effort", effort)
	encoder.StringKey("summary", "auto")
	encoder.EndObject()
	encoder.Key("include")
	encoder.BeginArray()
	encoder.String("reasoning.encrypted_content")
	encoder.EndArray()
}

func (p *Provider) SendsImages() bool { return true }

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	key, err := p.key(ctx)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, strings.TrimRight(p.cfg.BaseURL, "/")+"/models", nil)
	if err != nil {
		return nil, fmt.Errorf("openai: create models request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+key)
	resp, err := p.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("openai: models request: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		return nil, fmt.Errorf("openai: models http %d: %s", resp.StatusCode, bytes.TrimSpace(body))
	}
	var payload struct {
		Data []struct {
			ID string `json:"id"`
		} `json:"data"`
	}
	if err := json.UnmarshalRead(resp.Body, &payload); err != nil {
		return nil, fmt.Errorf("openai: decode models: %w", err)
	}
	models := make([]model.Model, 0, len(payload.Data))
	for _, item := range payload.Data {
		models = append(models, model.Model{ID: item.ID, Context: model.ContextWindow(item.ID, 0, p.cfg.ContextWindows), ToolChoice: true, Images: true})
	}
	return models, nil
}
