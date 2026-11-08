package anthropic

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"reflect"
	"sync/atomic"
	"testing"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/model"
)

type streamSummary struct {
	text      string
	reasoning string
	calls     []model.ToolCall
	usage     model.Usage
	stop      string
	failure   *model.Error
}

func TestStreamFixture(t *testing.T) {
	t.Parallel()
	fixture, err := os.ReadFile("testdata/basic.sse")
	if err != nil {
		t.Fatal(err)
	}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(fixture)
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "claude-test"})
	if summary.failure != nil {
		t.Fatalf("stream error: %v", summary.failure.Err)
	}
	if summary.text != "hello" || summary.reasoning != "think" {
		t.Fatalf("text = %q, reasoning = %q", summary.text, summary.reasoning)
	}
	assertFixtureCall(t, summary.calls)
	if summary.usage != (model.Usage{Input: 20, Output: 7, CacheRead: 3, CacheWrite: 2}) || summary.stop != "toolUse" {
		t.Fatalf("usage = %+v, stop = %q", summary.usage, summary.stop)
	}
}

func assertFixtureCall(t *testing.T, calls []model.ToolCall) {
	t.Helper()
	if len(calls) != 1 || calls[0].ID != "call-1" || calls[0].Name != "read" || string(calls[0].Args) != `{"path":"a"}` {
		t.Fatalf("calls = %+v", calls)
	}
}

func TestRequestBody(t *testing.T) {
	t.Parallel()
	var body []byte
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(successFixture())
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	request := model.Request{
		Model:  "claude-test",
		System: "system text",
		Messages: []model.Message{
			{Role: "user", Text: "first"},
			{Role: "assistant", Text: "prior", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{"path":"a"}`)}, {ID: "call-2", Name: "read", Args: jsontext.Value(`{"path":"b"}`)}}},
			{Role: "user", ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}},
			{Role: "user", ToolResults: []model.ToolResult{{ID: "call-2", Output: jsontext.Value(`"also done"`)}}},
		},
		Tools:     []model.Tool{{Name: "read", Parameters: jsontext.Value(`{"type":"object"}`)}},
		Reasoning: "high",
	}
	_ = collectStream(t, provider, request)
	assertRequestBody(t, body)
}

func TestFreeformToolRoundTrip(t *testing.T) {
	t.Parallel()
	var body []byte
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"p\",\"name\":\"apply_patch\",\"input\":{}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"input\\\":\\\"patch\\\"}\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	request := model.Request{
		Model:    "claude-test",
		Messages: []model.Message{{Role: "assistant", ToolCalls: []model.ToolCall{{ID: "p", Name: "apply_patch", Args: jsontext.Value("patch"), Freeform: true}}}},
		Tools:    []model.Tool{{Name: "apply_patch", Freeform: true}},
	}
	summary := collectStream(t, provider, request)
	if summary.failure != nil || len(summary.calls) != 1 || !summary.calls[0].Freeform || string(summary.calls[0].Args) != "patch" {
		t.Fatalf("summary = %+v", summary)
	}
	if !bytes.Contains(body, []byte(`"input_schema":{"type":"object"`)) || !bytes.Contains(body, []byte(`"input":{"input":"patch"}`)) {
		t.Fatalf("freeform request = %s", body)
	}
}

func TestUsageMergeIgnoresNull(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"cache_read_input_tokens\":3}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":null,\"output_tokens\":7,\"cache_read_input_tokens\":null}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "claude-test"})
	want := model.Usage{Input: 20, Output: 7, CacheRead: 3}
	if summary.failure != nil || summary.usage != want {
		t.Fatalf("summary = %+v, want usage %+v", summary, want)
	}
}

func TestModels(t *testing.T) {
	t.Parallel()
	mux := http.NewServeMux()
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("after_id") == "first" {
			_, _ = w.Write([]byte(`{"data":[{"id":"second","max_input_tokens":null,"max_tokens":8192,"capabilities":null}],"has_more":false,"last_id":"second"}`))
			return
		}
		_, _ = w.Write([]byte(`{"data":[{"id":"first","max_input_tokens":200000,"max_tokens":4096,"capabilities":{"effort":{"supported":true,"low":{"supported":true},"medium":{"supported":false},"high":{"supported":true},"xhigh":null,"max":{"supported":true}}}}],"has_more":true,"last_id":"first"}`))
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, ContextWindows: map[string]int{"second": 64000}})
	models, err := provider.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	want := []model.Model{
		{ID: "first", Context: 200000, MaxOutput: 4096, Efforts: []string{"low", "high", "max"}, ToolChoice: true},
		{ID: "second", Context: 64000, MaxOutput: 8192, ToolChoice: true},
	}
	if !reflect.DeepEqual(models, want) {
		t.Fatalf("models = %+v, want %+v", models, want)
	}
}

func TestMalformedEventRetainsBody(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("event: message_start\ndata: {\"type\":7}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry()})
	summary := collectStream(t, provider, model.Request{Model: "claude-test"})
	if summary.failure == nil || string(summary.failure.Body) != `{"type":7}` {
		t.Fatalf("failure = %+v", summary.failure)
	}
}

func TestRetryContract(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name        string
		handler     http.HandlerFunc
		wantRetries int
		wantError   bool
	}{
		{name: "two rate limits then success", handler: rateLimitHandler, wantRetries: 2},
		{name: "in-band rate limit", handler: inBandRateLimitHandler, wantRetries: 1},
		{name: "bad request", handler: badRequestHandler, wantError: true},
		{name: "tool call then drop", handler: toolDropHandler, wantError: true},
		{name: "headers then silence", handler: silentHandler, wantRetries: 3, wantError: true},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			runRetryScenario(t, test.handler, test.wantRetries, test.wantError)
		})
	}
}

func rateLimitHandler(w http.ResponseWriter, r *http.Request) {
	count := rateLimitRequests.Add(1)
	if count <= 2 {
		w.WriteHeader(http.StatusTooManyRequests)
		return
	}
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = w.Write(successFixture())
}

var rateLimitRequests atomic.Int32

func inBandRateLimitHandler(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "text/event-stream")
	if inBandRequests.Add(1) == 1 {
		_, _ = w.Write([]byte("{\"error\":{\"code\":429,\"message\":\"slow down\"}}\n"))
		return
	}
	_, _ = w.Write(successFixture())
}

var inBandRequests atomic.Int32

func badRequestHandler(w http.ResponseWriter, _ *http.Request) {
	w.WriteHeader(http.StatusBadRequest)
	_, _ = w.Write([]byte(`{"error":{"message":"bad"}}`))
}

func toolDropHandler(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = w.Write([]byte("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"c\",\"name\":\"read\",\"input\":{}}}\n\n"))
}

func silentHandler(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "text/event-stream")
	w.WriteHeader(http.StatusOK)
	if flusher, ok := w.(http.Flusher); ok {
		flusher.Flush()
	}
	<-r.Context().Done()
}

func runRetryScenario(t *testing.T, handler http.HandlerFunc, wantRetries int, wantError bool) {
	t.Helper()
	server := httptest.NewServer(handler)
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry()})
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	summary := collectStream(t, provider, model.Request{Model: "claude-test"})
	if retries != wantRetries || (summary.failure != nil) != wantError {
		t.Fatalf("retries = %d, failure = %+v; want %d, error %v", retries, summary.failure, wantRetries, wantError)
	}
}

func testKey(context.Context) (string, error) { return "test", nil }

func fastRetry() model.RetryOptions {
	return model.RetryOptions{
		StartTimeout: 100 * time.Millisecond,
		Sleep:        func(context.Context, time.Duration) error { return nil },
		Random:       func() float64 { return 0.5 },
	}
}

func successFixture() []byte {
	return []byte("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")
}

func collectStream(t *testing.T, provider *Provider, request model.Request) streamSummary {
	t.Helper()
	stream, err := provider.Stream(t.Context(), request)
	if err != nil {
		t.Fatal(err)
	}
	var summary streamSummary
	for event := range stream.Events() {
		switch value := event.(type) {
		case model.TextDelta:
			summary.text += value.Text
		case model.ReasoningDelta:
			summary.reasoning += value.Text
		case model.ToolCall:
			summary.calls = append(summary.calls, value)
		case model.Usage:
			summary.usage = value
		case model.Done:
			summary.stop = value.StopReason
		case model.Error:
			summary.failure = &value
		}
	}
	return summary
}

type requestContent struct {
	Type         string         `json:"type"`
	Text         string         `json:"text"`
	ID           string         `json:"id"`
	Name         string         `json:"name"`
	Input        jsontext.Value `json:"input"`
	ToolUseID    string         `json:"tool_use_id"`
	Content      jsontext.Value `json:"content"`
	CacheControl struct {
		Type string `json:"type"`
	} `json:"cache_control"`
}

type systemBlock struct {
	Type         string `json:"type"`
	Text         string `json:"text"`
	CacheControl struct {
		Type string `json:"type"`
	} `json:"cache_control"`
}

type requestMessage struct {
	Role    string           `json:"role"`
	Content []requestContent `json:"content"`
}

type requestPayload struct {
	MaxTokens int              `json:"max_tokens"`
	System    []systemBlock    `json:"system"`
	Messages  []requestMessage `json:"messages"`
	Thinking  struct {
		Type    string `json:"type"`
		Display string `json:"display"`
	} `json:"thinking"`
	OutputConfig struct {
		Effort string `json:"effort"`
	} `json:"output_config"`
}

func assertRequestBody(t *testing.T, body []byte) {
	t.Helper()
	var value requestPayload
	if err := json.Unmarshal(body, &value); err != nil {
		t.Fatalf("request JSON: %v\n%s", err, body)
	}
	assertSystemAndThinking(t, body, value)
	assertRequestMessages(t, value.Messages)
}

func assertSystemAndThinking(t *testing.T, body []byte, value requestPayload) {
	t.Helper()
	if len(value.System) != 1 || value.System[0].Text != "system text" || value.System[0].CacheControl.Type != "ephemeral" {
		t.Fatalf("system = %+v", value.System)
	}
	if value.MaxTokens != 4096 || value.Thinking.Type != "adaptive" || value.Thinking.Display != "summarized" || value.OutputConfig.Effort != "high" {
		t.Fatalf("max tokens = %d, thinking = %s, effort = %s", value.MaxTokens, value.Thinking.Type, value.OutputConfig.Effort)
	}
	if got := bytes.Count(body, []byte(`"cache_control":{"type":"ephemeral"}`)); got != 3 {
		t.Fatalf("cache markers = %d, want system, last tool, newest user\n%s", got, body)
	}
}

func assertRequestMessages(t *testing.T, messages []requestMessage) {
	t.Helper()
	if len(messages) != 3 || messages[1].Content[1].Type != "tool_use" {
		t.Fatalf("messages = %+v", messages)
	}
	last := messages[2].Content[len(messages[2].Content)-1]
	if messages[2].Content[0].CacheControl.Type != "" {
		t.Fatalf("earlier grouped result was cached = %+v", messages[2].Content[0])
	}
	if last.Type != "tool_result" || last.ToolUseID != "call-2" || string(last.Content) != `"also done"` || last.CacheControl.Type != "ephemeral" {
		t.Fatalf("newest user block = %+v", last)
	}
}
