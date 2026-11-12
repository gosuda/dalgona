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
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(fixture)
	})
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
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(successFixture())
	})
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
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"p\",\"name\":\"apply_patch\",\"input\":{}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"input\\\":\\\"patch\\\"}\"}}\n\nevent: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"))
	})
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
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"cache_read_input_tokens\":3}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":null,\"output_tokens\":7,\"cache_read_input_tokens\":null}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"))
	})
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
	server := newProviderOnlyServer(t, http.MethodGet, "/models", mux)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, ContextWindows: map[string]int{"second": 64000}})
	models, err := provider.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	want := []model.Model{
		{ID: "first", Context: 200000, MaxOutput: 4096, Efforts: []string{"low", "high", "max"}, ToolChoice: true, Images: true},
		{ID: "second", Context: 64000, MaxOutput: 8192, ToolChoice: true, Images: true},
	}
	if !reflect.DeepEqual(models, want) {
		t.Fatalf("models = %+v, want %+v", models, want)
	}
}

func TestMalformedEventRetainsBody(t *testing.T) {
	t.Parallel()
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("event: message_start\ndata: {\"type\":7}\n\n"))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry()})
	summary := collectStream(t, provider, model.Request{Model: "claude-test"})
	if summary.failure == nil || string(summary.failure.Body) != `{"type":7}` {
		t.Fatalf("failure = %+v", summary.failure)
	}
}

type retryScenario struct {
	name         string
	handler      func() http.HandlerFunc
	startTimeout time.Duration
	wantRetries  int
	wantError    bool
}

func TestRetryContract(t *testing.T) {
	t.Parallel()
	scenarios := []retryScenario{
		{name: "two rate limits then success", handler: rateLimitHandler, wantRetries: 2},
		{name: "in-band rate limit", handler: inBandRateLimitHandler, wantRetries: 1},
		{name: "bad request", handler: func() http.HandlerFunc { return badRequestHandler }, wantError: true},
		{name: "tool call then drop", handler: func() http.HandlerFunc { return toolDropHandler }, wantError: true},
		{name: "headers then silence", handler: func() http.HandlerFunc { return silentHandler }, startTimeout: 100 * time.Millisecond, wantRetries: 3, wantError: true},
	}
	for _, scenario := range scenarios {
		t.Run(scenario.name, func(t *testing.T) {
			runRetryScenario(t, scenario)
		})
	}
}

func rateLimitHandler() http.HandlerFunc {
	var requests atomic.Int32
	return func(w http.ResponseWriter, _ *http.Request) {
		if requests.Add(1) <= 2 {
			w.WriteHeader(http.StatusTooManyRequests)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(successFixture())
	}
}

func inBandRateLimitHandler() http.HandlerFunc {
	var requests atomic.Int32
	return func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		if requests.Add(1) == 1 {
			_, _ = w.Write([]byte("{\"error\":{\"code\":429,\"message\":\"slow down\"}}\n"))
			return
		}
		_, _ = w.Write(successFixture())
	}
}

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

func runRetryScenario(t *testing.T, scenario retryScenario) {
	t.Helper()
	server := newStreamServer(t, scenario.handler())
	retry := fastRetry()
	retry.StartTimeout = scenario.startTimeout
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: retry})
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	summary := collectStream(t, provider, model.Request{Model: "claude-test"})
	if retries != scenario.wantRetries || (summary.failure != nil) != scenario.wantError {
		t.Fatalf("retries = %d, failure = %+v; want %d, error %v", retries, summary.failure, scenario.wantRetries, scenario.wantError)
	}
}

func testKey(context.Context) (string, error) { return "test", nil }

func newStreamServer(t *testing.T, handler http.HandlerFunc) *httptest.Server {
	t.Helper()
	return newProviderOnlyServer(t, http.MethodPost, "/messages", handler)
}

func newProviderOnlyServer(t *testing.T, method, path string, handler http.Handler) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != method || r.URL.Path != path || r.Header.Get("x-api-key") != "test" {
			http.NotFound(w, r)
			return
		}
		handler.ServeHTTP(w, r)
	}))
	t.Cleanup(server.Close)
	return server
}

func fastRetry() model.RetryOptions {
	return model.RetryOptions{
		Sleep:  func(context.Context, time.Duration) error { return nil },
		Random: func() float64 { return 0.5 },
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

func TestEncodePassesXHighAndMaxEffort(t *testing.T) {
	t.Parallel()
	cases := []struct {
		reasoning string
		want      string
	}{
		{reasoning: "minimal", want: "low"},
		{reasoning: "low", want: "low"},
		{reasoning: "medium", want: "medium"},
		{reasoning: "high", want: "high"},
		{reasoning: "xhigh", want: "xhigh"},
		{reasoning: "max", want: "max"},
	}
	for _, tc := range cases {
		t.Run(tc.reasoning, func(t *testing.T) {
			t.Parallel()
			var body []byte
			server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
				body, _ = io.ReadAll(r.Body)
				w.Header().Set("Content-Type", "text/event-stream")
				_, _ = w.Write(successFixture())
			})
			provider := New(Config{BaseURL: server.URL, APIKey: testKey})
			_ = collectStream(t, provider, model.Request{Model: "claude-test", Reasoning: tc.reasoning})
			var value requestPayload
			if err := json.Unmarshal(body, &value); err != nil {
				t.Fatalf("request JSON: %v\n%s", err, body)
			}
			if value.OutputConfig.Effort != tc.want {
				t.Fatalf("reasoning %q sent effort %q, want %q", tc.reasoning, value.OutputConfig.Effort, tc.want)
			}
		})
	}
}

func TestEncodeRequestSendsImageBlock(t *testing.T) {
	t.Parallel()
	content := userContent(t, model.Message{Role: "user", Text: "What is this?", Images: []model.Image{{Mime: "image/png", Data: []byte("\x89PNG\r\n\x1a\n")}}})
	if len(content) != 2 {
		t.Fatalf("content = %d blocks, want 2: %s", len(content), content)
	}
	sameJSON(t, content[0], `{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}}`)
	sameJSON(t, content[1], `{"type":"text","text":"What is this?","cache_control":{"type":"ephemeral"}}`)
}

func TestEncodeRequestSendsImageOnlyMessage(t *testing.T) {
	t.Parallel()
	content := userContent(t, model.Message{Role: "user", Images: []model.Image{{Mime: "image/png", Data: []byte("\x89PNG\r\n\x1a\n")}}})
	if len(content) != 1 {
		t.Fatalf("content = %d blocks, want 1: %s", len(content), content)
	}
	sameJSON(t, content[0], `{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="},"cache_control":{"type":"ephemeral"}}`)
}

func TestEncodeRequestKeepsImagesWithToolResults(t *testing.T) {
	t.Parallel()
	content := userContent(t, model.Message{Role: "user", Images: []model.Image{{Mime: "image/png", Data: []byte("\x89PNG\r\n\x1a\n")}}, ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}})
	if len(content) != 2 {
		t.Fatalf("content = %d blocks, want 2: %s", len(content), content)
	}
	sameJSON(t, content[0], `{"type":"image","source":{"type":"base64","media_type":"image/png","data":"iVBORw0KGgo="}}`)
	sameJSON(t, content[1], `{"type":"tool_result","tool_use_id":"call-1","content":"done","cache_control":{"type":"ephemeral"}}`)
}

func userContent(t *testing.T, message model.Message) []jsontext.Value {
	t.Helper()
	var body []byte
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(successFixture())
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	_ = collectStream(t, provider, model.Request{Model: "claude-test", Messages: []model.Message{message}})
	var value struct {
		Messages []struct {
			Content []jsontext.Value `json:"content"`
		} `json:"messages"`
	}
	if err := json.Unmarshal(body, &value); err != nil {
		t.Fatalf("request JSON: %v\n%s", err, body)
	}
	if len(value.Messages) != 1 {
		t.Fatalf("messages = %d, want 1: %s", len(value.Messages), body)
	}
	return value.Messages[0].Content
}

func sameJSON(t *testing.T, got jsontext.Value, want string) {
	t.Helper()
	var g, w any
	if err := json.Unmarshal(got, &g); err != nil {
		t.Fatalf("block %s: %v", got, err)
	}
	if err := json.Unmarshal([]byte(want), &w); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(g, w) {
		t.Fatalf("block = %s, want %s", got, want)
	}
}
