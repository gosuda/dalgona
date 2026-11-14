package google

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"slices"
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
		cfg.BaseURL = "https://generativelanguage.googleapis.com"
	}
	return &Provider{cfg: cfg, client: model.NewHTTPClient(cfg.Client)}
}

func APIKeyFromEnv(ctx context.Context) (string, error) {
	if err := ctx.Err(); err != nil {
		return "", err
	}
	key := os.Getenv("GEMINI_API_KEY")
	if key == "" {
		return "", errors.New("GEMINI_API_KEY is not set")
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
	body, err := EncodeRequest(req)
	if err != nil {
		return model.AttemptResult{Err: err}
	}
	endpoint := strings.TrimRight(p.cfg.BaseURL, "/") + "/v1beta/models/" + url.PathEscape(req.Model) + ":streamGenerateContent?alt=sse"
	gateCtx, gate := model.NewStartGate(ctx, p.cfg.Retry.StartTimeout)
	defer gate.Close()
	httpReq, err := http.NewRequestWithContext(gateCtx, http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return model.AttemptResult{Err: fmt.Errorf("google: create request: %w", err)}
	}
	httpReq.Header.Set("x-goog-api-key", key)
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Accept", "text/event-stream")
	resp, err := p.client.Do(httpReq)
	if err != nil {
		return model.AttemptResult{Err: gate.Error(gateCtx, fmt.Errorf("google: request: %w", err))}
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return model.HTTPFailure("google", resp)
	}
	return NewDecoder(stream, req.Tools).consume(gateCtx, gate, resp.Body, p.cfg.Retry)
}

func (p *Provider) key(ctx context.Context) (string, error) {
	if p.cfg.APIKey != nil {
		return p.cfg.APIKey(ctx)
	}
	return APIKeyFromEnv(ctx)
}

func EncodeRequest(req model.Request) ([]byte, error) {
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	encoder.BeginObject()
	encodeSystem(encoder, req.System)
	encodeContents(encoder, req.Messages)
	if err := encodeTools(encoder, req.Tools); err != nil {
		return nil, err
	}
	encodeGeneration(encoder, req.Reasoning)
	encoder.EndObject()
	if err := encoder.Err(); err != nil {
		return nil, fmt.Errorf("google: encode request: %w", err)
	}
	return buffer.Bytes(), nil
}

func encodeSystem(encoder *model.JSONEncoder, system string) {
	if system == "" {
		return
	}
	encoder.Key("systemInstruction")
	encoder.BeginObject()
	encoder.Key("parts")
	encoder.BeginArray()
	encoder.BeginObject()
	encoder.StringKey("text", system)
	encoder.EndObject()
	encoder.EndArray()
	encoder.EndObject()
}

func encodeContents(encoder *model.JSONEncoder, messages []model.Message) {
	encoder.Key("contents")
	encoder.BeginArray()
	names := callNames(messages)
	for _, message := range messages {
		encodeContent(encoder, message, names)
	}
	encoder.EndArray()
}

func callNames(messages []model.Message) map[string]string {
	names := make(map[string]string)
	for _, message := range messages {
		for _, call := range message.ToolCalls {
			names[call.ID] = call.Name
		}
	}
	return names
}

func encodeContent(encoder *model.JSONEncoder, message model.Message, names map[string]string) {
	if message.Role == "assistant" {
		encodeModelContent(encoder, message)
		if len(message.ToolResults) != 0 {
			encodeToolResults(encoder, message.ToolResults, names)
		}
		return
	}
	if message.Text != "" || len(message.Images) != 0 {
		encoder.BeginObject()
		encoder.StringKey("role", "user")
		encoder.Key("parts")
		encoder.BeginArray()
		for _, image := range message.Images {
			encodeImagePart(encoder, image)
		}
		if message.Text != "" {
			encodeTextPart(encoder, message.Text, false)
		}
		encoder.EndArray()
		encoder.EndObject()
	}
	encodeToolResults(encoder, message.ToolResults, names)
}

func encodeImagePart(encoder *model.JSONEncoder, image model.Image) {
	encoder.BeginObject()
	encoder.Key("inlineData")
	encoder.BeginObject()
	encoder.StringKey("mimeType", image.Mime)
	encoder.StringKey("data", base64.StdEncoding.EncodeToString(image.Data))
	encoder.EndObject()
	encoder.EndObject()
}

func encodeModelContent(encoder *model.JSONEncoder, message model.Message) {
	if message.Text == "" && message.Reasoning == "" && len(message.ToolCalls) == 0 {
		return
	}
	encoder.BeginObject()
	encoder.StringKey("role", "model")
	encoder.Key("parts")
	encoder.BeginArray()
	if message.Reasoning != "" {
		encodeTextPart(encoder, message.Reasoning, true)
	}
	if message.Text != "" {
		encodeTextPart(encoder, message.Text, false)
	}
	for _, call := range message.ToolCalls {
		encodeFunctionCall(encoder, call)
	}
	encoder.EndArray()
	encoder.EndObject()
}

func encodeTextPart(encoder *model.JSONEncoder, text string, thought bool) {
	encoder.BeginObject()
	encoder.StringKey("text", text)
	if thought {
		encoder.BoolKey("thought", true)
	}
	encoder.EndObject()
}

func encodeFunctionCall(encoder *model.JSONEncoder, call model.ToolCall) {
	encoder.BeginObject()
	encoder.Key("functionCall")
	encoder.BeginObject()
	encoder.StringKey("id", call.ID)
	encoder.StringKey("name", call.Name)
	if call.Freeform {
		encoder.Key("args")
		encoder.BeginObject()
		encoder.StringKey("input", string(call.Args))
		encoder.EndObject()
	} else {
		encoder.ValueKey("args", call.Args)
	}
	encoder.EndObject()
	if call.Signature != "" {
		encoder.StringKey("thoughtSignature", call.Signature)
	}
	encoder.EndObject()
}

func encodeToolResults(encoder *model.JSONEncoder, results []model.ToolResult, names map[string]string) {
	if len(results) == 0 {
		return
	}
	encoder.BeginObject()
	encoder.StringKey("role", "user")
	encoder.Key("parts")
	encoder.BeginArray()
	for _, result := range results {
		encodeFunctionResponse(encoder, result, names)
	}
	encoder.EndArray()
	encoder.EndObject()
}

func encodeFunctionResponse(encoder *model.JSONEncoder, result model.ToolResult, names map[string]string) {
	name := result.Name
	if name == "" {
		name = names[result.ID]
	}
	encoder.BeginObject()
	encoder.Key("functionResponse")
	encoder.BeginObject()
	encoder.StringKey("id", result.ID)
	encoder.StringKey("name", name)
	encoder.Key("response")
	encoder.BeginObject()
	if result.IsError {
		encoder.StringKey("error", model.ToolResultText(result.Output))
	} else {
		encoder.StringKey("output", model.ToolResultText(result.Output))
	}
	encoder.EndObject()
	encoder.EndObject()
	encoder.EndObject()
}

func encodeTools(encoder *model.JSONEncoder, tools []model.Tool) error {
	if len(tools) == 0 {
		return nil
	}
	encoder.Key("tools")
	encoder.BeginArray()
	encoder.BeginObject()
	encoder.Key("functionDeclarations")
	encoder.BeginArray()
	for _, tool := range tools {
		if err := encodeDeclaration(encoder, tool); err != nil {
			return err
		}
	}
	encoder.EndArray()
	encoder.EndObject()
	encoder.EndArray()
	return nil
}

func encodeDeclaration(encoder *model.JSONEncoder, tool model.Tool) error {
	encoder.BeginObject()
	encoder.StringKey("name", tool.Name)
	if tool.Description != "" {
		encoder.StringKey("description", tool.Description)
	}
	encoder.Key("parametersJsonSchema")
	if tool.Freeform {
		encoder.FreeformSchema()
	} else if err := encodeSchema(encoder, tool.JSONSchema()); err != nil {
		return err
	}
	encoder.EndObject()
	return nil
}

func encodeSchema(encoder *model.JSONEncoder, schema jsontext.Value) error {
	decoder := jsontext.NewDecoder(bytes.NewReader(schema))
	if err := encodeSchemaNode(encoder, decoder); err != nil {
		return err
	}
	return model.RequireJSONEOF(decoder)
}

var schemaMapKeywords = map[string]bool{
	"properties": true, "patternProperties": true, "$defs": true, "definitions": true, "dependentSchemas": true,
}

var schemaNodeKeywords = map[string]bool{
	"items": true, "prefixItems": true, "additionalItems": true, "additionalProperties": true,
	"unevaluatedItems": true, "unevaluatedProperties": true, "contains": true, "propertyNames": true,
	"not": true, "if": true, "then": true, "else": true, "anyOf": true, "allOf": true, "oneOf": true,
}

func encodeSchemaNode(encoder *model.JSONEncoder, decoder *jsontext.Decoder) error {
	kind := decoder.PeekKind()
	if kind == jsontext.KindBeginObject {
		return encodeSchemaObject(encoder, decoder)
	}
	if kind == jsontext.KindBeginArray {
		return encodeSchemaList(encoder, decoder)
	}
	return copySchemaValue(encoder, decoder)
}

func copySchemaValue(encoder *model.JSONEncoder, decoder *jsontext.Decoder) error {
	value, err := decoder.ReadValue()
	if err != nil {
		return fmt.Errorf("google: schema value: %w", err)
	}
	encoder.Value(value)
	return nil
}

func encodeSchemaObject(encoder *model.JSONEncoder, decoder *jsontext.Decoder) error {
	return encodeSchemaFields(encoder, decoder, encodeSchemaKeyword)
}

func encodeSchemaMap(encoder *model.JSONEncoder, decoder *jsontext.Decoder) error {
	return encodeSchemaFields(encoder, decoder, func(encoder *model.JSONEncoder, decoder *jsontext.Decoder, name string) error {
		encoder.Key(name)
		return encodeSchemaNode(encoder, decoder)
	})
}

func encodeSchemaKeyword(encoder *model.JSONEncoder, decoder *jsontext.Decoder, name string) error {
	switch {
	case name == "const" && scalarKind(decoder.PeekKind()):
		encoder.Key("enum")
		encoder.BeginArray()
		err := copySchemaValue(encoder, decoder)
		encoder.EndArray()
		return err
	case schemaMapKeywords[name] && decoder.PeekKind() == '{':
		encoder.Key(name)
		return encodeSchemaMap(encoder, decoder)
	case schemaNodeKeywords[name]:
		encoder.Key(name)
		return encodeSchemaNode(encoder, decoder)
	default:
		encoder.Key(name)
		return copySchemaValue(encoder, decoder)
	}
}

func encodeSchemaFields(encoder *model.JSONEncoder, decoder *jsontext.Decoder, field func(*model.JSONEncoder, *jsontext.Decoder, string) error) error {
	if _, err := decoder.ReadToken(); err != nil {
		return fmt.Errorf("google: schema object start: %w", err)
	}
	encoder.BeginObject()
	for decoder.PeekKind() != '}' {
		token, err := decoder.ReadToken()
		if err != nil {
			return fmt.Errorf("google: schema name: %w", err)
		}
		if err := field(encoder, decoder, token.String()); err != nil {
			return err
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return fmt.Errorf("google: schema object end: %w", err)
	}
	encoder.EndObject()
	return nil
}

func scalarKind(kind jsontext.Kind) bool {
	switch kind {
	case jsontext.KindNull, jsontext.KindFalse, jsontext.KindTrue, jsontext.KindString, jsontext.KindNumber:
		return true
	case jsontext.KindInvalid, jsontext.KindBeginObject, jsontext.KindEndObject, jsontext.KindBeginArray, jsontext.KindEndArray:
		return false
	}
	return false
}

func encodeSchemaList(encoder *model.JSONEncoder, decoder *jsontext.Decoder) error {
	if _, err := decoder.ReadToken(); err != nil {
		return fmt.Errorf("google: schema array start: %w", err)
	}
	encoder.BeginArray()
	for decoder.PeekKind() != ']' {
		if err := encodeSchemaNode(encoder, decoder); err != nil {
			return err
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return fmt.Errorf("google: schema array end: %w", err)
	}
	encoder.EndArray()
	return nil
}

func encodeGeneration(encoder *model.JSONEncoder, reasoning string) {
	encoder.Key("generationConfig")
	encoder.BeginObject()
	if reasoning != "" && reasoning != "default" {
		encoder.Key("thinkingConfig")
		encoder.BeginObject()
		encoder.BoolKey("includeThoughts", true)
		encoder.StringKey("thinkingLevel", googleEffort(reasoning))
		encoder.EndObject()
	}
	encoder.EndObject()
}

func googleEffort(reasoning string) string {
	switch reasoning {
	case "minimal":
		return "MINIMAL"
	case "low":
		return "LOW"
	case "medium":
		return "MEDIUM"
	default:
		return "HIGH"
	}
}

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	var models []model.Model
	seen := make(map[string]bool)
	pageToken := ""
	for {
		page, err := p.modelsPage(ctx, pageToken)
		if err != nil {
			return nil, err
		}
		models = p.appendModels(models, page)
		if page.NextPageToken == "" {
			return models, nil
		}
		if seen[page.NextPageToken] {
			return nil, fmt.Errorf("google: models pagination repeated token %s", page.NextPageToken)
		}
		seen[page.NextPageToken] = true
		pageToken = page.NextPageToken
	}
}

type modelListPage struct {
	Models []struct {
		Name    string   `json:"name"`
		Input   int      `json:"inputTokenLimit"`
		Output  int      `json:"outputTokenLimit"`
		Methods []string `json:"supportedGenerationMethods"`
	} `json:"models"`
	NextPageToken string `json:"nextPageToken"`
}

func (p *Provider) modelsPage(ctx context.Context, pageToken string) (modelListPage, error) {
	var page modelListPage
	key, err := p.key(ctx)
	if err != nil {
		return page, err
	}
	endpoint := strings.TrimRight(p.cfg.BaseURL, "/") + "/v1beta/models"
	if pageToken != "" {
		endpoint += "?pageToken=" + url.QueryEscape(pageToken)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return page, fmt.Errorf("google: create models request: %w", err)
	}
	req.Header.Set("x-goog-api-key", key)
	resp, err := p.client.Do(req)
	if err != nil {
		return page, fmt.Errorf("google: models request: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		body, readErr := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
		if readErr != nil {
			return page, fmt.Errorf("google: read models error response: %w", readErr)
		}
		return page, fmt.Errorf("google: models http %d: %s", resp.StatusCode, bytes.TrimSpace(body))
	}
	if err := json.UnmarshalRead(resp.Body, &page); err != nil {
		return page, fmt.Errorf("google: decode models: %w", err)
	}
	return page, nil
}

func (p *Provider) appendModels(models []model.Model, page modelListPage) []model.Model {
	for _, item := range page.Models {
		if !supportsGenerateContent(item.Methods) {
			continue
		}
		id := strings.TrimPrefix(item.Name, "models/")
		models = append(models, model.Model{
			ID:         id,
			Context:    model.ContextWindow(id, item.Input, p.cfg.ContextWindows),
			MaxOutput:  item.Output,
			ToolChoice: true,
			Images:     true,
		})
	}
	return models
}

func supportsGenerateContent(methods []string) bool {
	return len(methods) == 0 || slices.Contains(methods, "generateContent")
}
