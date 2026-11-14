package anthropic

import (
	"bytes"
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
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
		cfg.BaseURL = "https://api.anthropic.com/v1"
	}
	return &Provider{cfg: cfg, client: model.NewHTTPClient(cfg.Client)}
}

func APIKeyFromEnv(ctx context.Context) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	key := os.Getenv("ANTHROPIC_API_KEY")
	if key == "" {
		return "", errors.New("ANTHROPIC_API_KEY is not set")
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
	endpoint := strings.TrimRight(p.cfg.BaseURL, "/") + "/messages"
	gateCtx, gate := model.NewStartGate(ctx, p.cfg.Retry.StartTimeout)
	defer gate.Close()
	httpReq, err := http.NewRequestWithContext(gateCtx, http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return model.AttemptResult{Err: fmt.Errorf("anthropic: create request: %w", err)}
	}
	httpReq.Header.Set("x-api-key", key)
	httpReq.Header.Set("anthropic-version", "2023-06-01")
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Accept", "text/event-stream")
	resp, err := p.client.Do(httpReq)
	if err != nil {
		return model.AttemptResult{Err: gate.Error(gateCtx, fmt.Errorf("anthropic: request: %w", err))}
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return model.HTTPFailure("anthropic", resp)
	}
	state := responseState{stream: stream, freeform: model.FreeformToolNames(req.Tools)}
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
	encoder.IntKey("max_tokens", 4096)
	encodeSystem(encoder, req.System)
	encodeMessages(encoder, req.Messages, req.Reasoning)
	encodeTools(encoder, req.Tools)
	encoder.BoolKey("stream", true)
	encodeThinking(encoder, req.Reasoning)
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return nil, fmt.Errorf("anthropic: encode request: %w", err)
	}
	return buffer.Bytes(), nil
}

func encodeSystem(encoder *model.JSONEncoder, system string) {
	if system == "" {
		return
	}
	encoder.Key("system")
	encoder.BeginArray()
	encodeTextBlock(encoder, system, true)
	encoder.EndArray()
}

func encodeMessages(encoder *model.JSONEncoder, messages []model.Message, reasoning string) {
	encoder.Key("messages")
	encoder.BeginArray()
	lastUser := lastUserMessage(messages)
	for index := 0; index < len(messages); index++ {
		if toolResultsOnly(messages[index]) {
			end := toolResultGroupEnd(messages, index)
			encodeToolResultGroup(encoder, messages[index:end+1], end == lastUser)
			index = end
			continue
		}
		encodeMessage(encoder, messages[index], index == lastUser, reasoning)
	}
	encoder.EndArray()
}

func lastUserMessage(messages []model.Message) int {
	last := -1
	for index, message := range messages {
		if message.Role == "user" {
			last = index
		}
	}
	return last
}

func toolResultsOnly(message model.Message) bool {
	return message.Role == "user" && message.Text == "" && len(message.ToolCalls) == 0 && len(message.ToolResults) != 0 && len(message.Images) == 0
}

func toolResultGroupEnd(messages []model.Message, start int) int {
	end := start
	for end+1 < len(messages) && toolResultsOnly(messages[end+1]) {
		end++
	}
	return end
}

func encodeMessage(encoder *model.JSONEncoder, message model.Message, cache bool, reasoning string) {
	if message.Role == "assistant" {
		encodeAssistant(encoder, message, reasoning)
		if len(message.ToolResults) != 0 {
			encodeToolResultGroup(encoder, []model.Message{message}, cache)
		}
		return
	}
	if message.Text == "" && len(message.ToolResults) == 0 && len(message.Images) == 0 {
		return
	}
	encoder.BeginObject()
	encoder.StringKey("role", "user")
	encoder.Key("content")
	encoder.BeginArray()
	encodeUserContent(encoder, message, cache)
	encoder.EndArray()
	encoder.EndObject()
}

func encodeUserContent(encoder *model.JSONEncoder, message model.Message, cache bool) {
	final := cache && len(message.ToolResults) == 0
	if final && message.Text == "" {
		encodeCachedImages(encoder, message.Images)
	} else {
		encodeImages(encoder, message.Images)
	}
	if message.Text != "" {
		encodeTextBlock(encoder, message.Text, final)
	}
	encodeResults(encoder, message.ToolResults, cache)
}

func encodeImages(encoder *model.JSONEncoder, images []model.Image) {
	for _, image := range images {
		encodeImageBlock(encoder, image)
	}
}

func encodeCachedImages(encoder *model.JSONEncoder, images []model.Image) {
	last := len(images) - 1
	for index, image := range images {
		if index == last {
			encodeCachedImageBlock(encoder, image)
			continue
		}
		encodeImageBlock(encoder, image)
	}
}

func encodeAssistant(encoder *model.JSONEncoder, message model.Message, reasoning string) {
	signature, redacted := replayFor(message, reasoning)
	if message.Text == "" && len(message.ToolCalls) == 0 && signature == "" && len(redacted) == 0 {
		return
	}
	encoder.BeginObject()
	encoder.StringKey("role", "assistant")
	encoder.Key("content")
	encoder.BeginArray()
	if signature != "" {
		encodeThinkingBlock(encoder, message.Reasoning, signature)
	}
	for _, data := range redacted {
		encoder.BeginObject()
		encoder.StringKey("type", "redacted_thinking")
		encoder.StringKey("data", data)
		encoder.EndObject()
	}
	if message.Text != "" {
		encodeTextBlock(encoder, message.Text, false)
	}
	for _, call := range message.ToolCalls {
		encodeToolUse(encoder, call)
	}
	encoder.EndArray()
	encoder.EndObject()
}

func replayFor(message model.Message, reasoning string) (string, []string) {
	if !thinkingEnabled(reasoning) {
		return "", nil
	}
	var signature string
	var redacted []string
	for _, call := range message.ToolCalls {
		if signature == "" && message.Reasoning != "" {
			signature = call.Signature
		}
		if len(redacted) == 0 {
			redacted = call.RedactedThinking
		}
	}
	return signature, redacted
}

func encodeThinkingBlock(encoder *model.JSONEncoder, reasoning, signature string) {
	encoder.BeginObject()
	encoder.StringKey("type", "thinking")
	encoder.StringKey("thinking", reasoning)
	encoder.StringKey("signature", signature)
	encoder.EndObject()
}

func encodeTextBlock(encoder *model.JSONEncoder, text string, cache bool) {
	encoder.BeginObject()
	encoder.StringKey("type", "text")
	encoder.StringKey("text", text)
	encodeCacheControl(encoder, cache)
	encoder.EndObject()
}

func encodeImageBlock(encoder *model.JSONEncoder, image model.Image) {
	encoder.BeginObject()
	writeImageFields(encoder, image)
	encoder.EndObject()
}

func encodeCachedImageBlock(encoder *model.JSONEncoder, image model.Image) {
	encoder.BeginObject()
	writeImageFields(encoder, image)
	encodeCacheControl(encoder, true)
	encoder.EndObject()
}

func writeImageFields(encoder *model.JSONEncoder, image model.Image) {
	encoder.StringKey("type", "image")
	encoder.Key("source")
	encoder.BeginObject()
	encoder.StringKey("type", "base64")
	encoder.StringKey("media_type", image.Mime)
	encoder.StringKey("data", base64.StdEncoding.EncodeToString(image.Data))
	encoder.EndObject()
}

func encodeCacheControl(encoder *model.JSONEncoder, cache bool) {
	if !cache {
		return
	}
	encoder.Key("cache_control")
	encoder.BeginObject()
	encoder.StringKey("type", "ephemeral")
	encoder.EndObject()
}

func encodeToolUse(encoder *model.JSONEncoder, call model.ToolCall) {
	encoder.BeginObject()
	encoder.StringKey("type", "tool_use")
	encoder.StringKey("id", call.ID)
	encoder.StringKey("name", call.Name)
	encoder.Key("input")
	if call.Freeform {
		encoder.BeginObject()
		encoder.StringKey("input", string(call.Args))
		encoder.EndObject()
	} else {
		encoder.Value(call.Args)
	}
	encoder.EndObject()
}

func encodeToolResultGroup(encoder *model.JSONEncoder, messages []model.Message, cache bool) {
	encoder.BeginObject()
	encoder.StringKey("role", "user")
	encoder.Key("content")
	encoder.BeginArray()
	for index, message := range messages {
		encodeResults(encoder, message.ToolResults, cache && index == len(messages)-1)
	}
	encoder.EndArray()
	encoder.EndObject()
}

func encodeResults(encoder *model.JSONEncoder, results []model.ToolResult, cache bool) {
	for index, result := range results {
		encoder.BeginObject()
		encoder.StringKey("type", "tool_result")
		encoder.StringKey("tool_use_id", result.ID)
		encoder.StringKey("content", model.ToolResultText(result.Output))
		if result.IsError {
			encoder.BoolKey("is_error", true)
		}
		encodeCacheControl(encoder, cache && index == len(results)-1)
		encoder.EndObject()
	}
}

func encodeTools(encoder *model.JSONEncoder, tools []model.Tool) {
	if len(tools) == 0 {
		return
	}
	encoder.Key("tools")
	encoder.BeginArray()
	for index, tool := range tools {
		encoder.BeginObject()
		encoder.StringKey("name", tool.Name)
		if tool.Description != "" {
			encoder.StringKey("description", tool.Description)
		}
		encoder.Key("input_schema")
		if tool.Freeform {
			encoder.FreeformSchema()
		} else {
			encoder.Value(tool.JSONSchema())
		}
		encodeCacheControl(encoder, index == len(tools)-1)
		encoder.EndObject()
	}
	encoder.EndArray()
}

func encodeThinking(encoder *model.JSONEncoder, reasoning string) {
	if !thinkingEnabled(reasoning) {
		return
	}
	encoder.Key("thinking")
	encoder.BeginObject()
	encoder.StringKey("type", "adaptive")
	encoder.StringKey("display", "summarized")
	encoder.EndObject()
	encoder.Key("output_config")
	encoder.BeginObject()
	encoder.StringKey("effort", anthropicEffort(reasoning))
	encoder.EndObject()
}

func thinkingEnabled(reasoning string) bool {
	return reasoning != "" && reasoning != "default"
}

func anthropicEffort(reasoning string) string {
	switch reasoning {
	case "minimal", "low":
		return "low"
	case "medium", "high", "xhigh", "max":
		return reasoning
	default:
		return "high"
	}
}

type effortSupport struct {
	Supported bool `json:"supported"`
}

type effortCapabilities struct {
	Supported bool          `json:"supported"`
	Low       effortSupport `json:"low"`
	Medium    effortSupport `json:"medium"`
	High      effortSupport `json:"high"`
	XHigh     effortSupport `json:"xhigh"`
	Max       effortSupport `json:"max"`
}

type modelListPage struct {
	Data []struct {
		ID           string `json:"id"`
		MaxInput     int    `json:"max_input_tokens"`
		MaxOutput    int    `json:"max_tokens"`
		Capabilities struct {
			Effort effortCapabilities `json:"effort"`
		} `json:"capabilities"`
	} `json:"data"`
	HasMore bool   `json:"has_more"`
	LastID  string `json:"last_id"`
}

func supportedEfforts(capabilities effortCapabilities) []string {
	if !capabilities.Supported {
		return nil
	}
	levels := []struct {
		name    string
		support effortSupport
	}{
		{name: "low", support: capabilities.Low},
		{name: "medium", support: capabilities.Medium},
		{name: "high", support: capabilities.High},
		{name: "xhigh", support: capabilities.XHigh},
		{name: "max", support: capabilities.Max},
	}
	var efforts []string
	for _, level := range levels {
		if level.support.Supported {
			efforts = append(efforts, level.name)
		}
	}
	return efforts
}

func (p *Provider) SendsImages() bool { return true }

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	var models []model.Model
	seen := make(map[string]bool)
	afterID := ""
	for {
		page, err := p.modelsPage(ctx, afterID)
		if err != nil {
			return nil, err
		}
		for _, item := range page.Data {
			models = append(models, model.Model{
				ID:         item.ID,
				Context:    model.ContextWindow(item.ID, item.MaxInput, p.cfg.ContextWindows),
				MaxOutput:  item.MaxOutput,
				Efforts:    supportedEfforts(item.Capabilities.Effort),
				ToolChoice: true,
				Images:     true,
			})
		}
		if !page.HasMore || page.LastID == "" {
			return models, nil
		}
		afterID = page.LastID
		if seen[afterID] {
			return nil, fmt.Errorf("anthropic: models pagination repeated cursor %s", afterID)
		}
		seen[afterID] = true
	}
}

func (p *Provider) modelsPage(ctx context.Context, afterID string) (modelListPage, error) {
	var page modelListPage
	key, err := p.key(ctx)
	if err != nil {
		return page, err
	}
	endpoint := strings.TrimRight(p.cfg.BaseURL, "/") + "/models"
	if afterID != "" {
		endpoint += "?after_id=" + url.QueryEscape(afterID)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return page, fmt.Errorf("anthropic: create models request: %w", err)
	}
	req.Header.Set("x-api-key", key)
	req.Header.Set("anthropic-version", "2023-06-01")
	resp, err := p.client.Do(req)
	if err != nil {
		return page, fmt.Errorf("anthropic: models request: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, readErr := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		if readErr != nil {
			return page, fmt.Errorf("anthropic: read models error response: %w", readErr)
		}
		return page, fmt.Errorf("anthropic: models http %d: %s", resp.StatusCode, bytes.TrimSpace(body))
	}
	if err := json.UnmarshalRead(resp.Body, &page); err != nil {
		return page, fmt.Errorf("anthropic: decode models: %w", err)
	}
	return page, nil
}
