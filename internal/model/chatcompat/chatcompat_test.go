package chatcompat

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
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
	if summary.failure != nil {
		t.Fatalf("stream error: %v", summary.failure.Err)
	}
	if summary.text != "hello" || summary.reasoning != "think" {
		t.Fatalf("text = %q, reasoning = %q", summary.text, summary.reasoning)
	}
	assertFixtureCalls(t, summary.calls)
	if summary.usage != (model.Usage{Input: 15, Output: 7, CacheRead: 3, CacheWrite: 2}) || summary.stop != "toolUse" {
		t.Fatalf("usage = %+v, stop = %q", summary.usage, summary.stop)
	}
}

func assertFixtureCalls(t *testing.T, calls []model.ToolCall) {
	t.Helper()
	if len(calls) != 2 || calls[0].ID != "call-1" || string(calls[0].Args) != `{"path":"a"}` {
		t.Fatalf("calls = %+v", calls)
	}
	if calls[1].ID != "call-2" || calls[1].Name != "exec" || string(calls[1].Args) != `{"command":"pwd"}` {
		t.Fatalf("second call = %+v", calls[1])
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
		Model:  "test-model",
		System: "system text",
		Messages: []model.Message{
			{Role: "assistant", Text: "prior", Reasoning: "why", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{"path":"a"}`)}}},
			{Role: "user", Text: "after tools", ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}},
		},
		Tools: []model.Tool{
			{Name: "read", Parameters: jsontext.Value(`{"type":"object"}`)},
			{Name: "apply_patch", Freeform: true},
		},
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
		_, _ = w.Write([]byte("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"p\",\"function\":{\"name\":\"apply_patch\",\"arguments\":\"{\\\"input\\\":\\\"patch\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	request := model.Request{
		Model:    "test-model",
		Messages: []model.Message{{Role: "assistant", ToolCalls: []model.ToolCall{{ID: "p", Name: "apply_patch", Args: jsontext.Value("patch"), Freeform: true}}}},
		Tools:    []model.Tool{{Name: "apply_patch", Freeform: true}},
	}
	summary := collectStream(t, provider, request)
	if summary.failure != nil || len(summary.calls) != 1 || !summary.calls[0].Freeform || string(summary.calls[0].Args) != "patch" {
		t.Fatalf("summary = %+v", summary)
	}
	if !bytes.Contains(body, []byte(`"arguments":"{\"input\":\"patch\"}"`)) {
		t.Fatalf("freeform history = %s", body)
	}
}

func TestUsageFallback(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		body string
		want model.Usage
	}{
		{name: "top-level usage wins", body: "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\",\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":7}}\n\n", want: model.Usage{Input: 20, Output: 7}},
		{name: "choice usage is fallback", body: "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\",\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}]}\n\n", want: model.Usage{Input: 5, Output: 2}},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "text/event-stream")
				_, _ = w.Write([]byte(test.body))
			}))
			t.Cleanup(server.Close)
			provider := New(Config{BaseURL: server.URL, APIKey: testKey})
			summary := collectStream(t, provider, model.Request{Model: "test-model"})
			if summary.failure != nil || summary.usage != test.want {
				t.Fatalf("summary = %+v, want usage %+v", summary, test.want)
			}
		})
	}
}

func TestUsageCachePrecedence(t *testing.T) {
	t.Parallel()
	body := "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":4,\"cache_write_tokens\":1},\"cached_tokens\":9}}\n\n"
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(body))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
	want := model.Usage{Input: 15, Output: 7, CacheRead: 4, CacheWrite: 1}
	if summary.failure != nil || summary.usage != want {
		t.Fatalf("summary = %+v, want usage %+v", summary, want)
	}
}

func TestReasoningPrecedenceSkipsNonStrings(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"\",\"reasoning\":{\"thinking\":\"nested\"},\"reasoning_text\":\"fallback\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
	if summary.failure != nil || summary.reasoning != "fallback" {
		t.Fatalf("summary = %+v", summary)
	}
}

func TestRawJSONToolArguments(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"r\",\"function\":{\"name\":\"read\",\"arguments\":{\"path\":\"a\", \"limit\":2}}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
	if summary.failure != nil || len(summary.calls) != 1 || string(summary.calls[0].Args) != `{"path":"a", "limit":2}` {
		t.Fatalf("summary = %+v", summary)
	}
}

func TestModels(t *testing.T) {
	t.Parallel()
	mux := http.NewServeMux()
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(`{"data":[{"id":"large","context_length":200000,"top_provider":{"context_length":100000,"max_completion_tokens":8192}},{"id":"routed","context_length":null,"top_provider":{"context_length":32000,"max_completion_tokens":null}},{"id":"configured"},{"id":"basic"}]}`))
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, ContextWindows: map[string]int{"configured": 64000}})
	models, err := provider.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	want := []model.Model{
		{ID: "large", Context: 200000, MaxOutput: 8192, ToolChoice: true},
		{ID: "routed", Context: 32000, ToolChoice: true},
		{ID: "configured", Context: 64000, ToolChoice: true},
		{ID: "basic", Context: 128000, ToolChoice: true},
	}
	if !reflect.DeepEqual(models, want) {
		t.Fatalf("models = %+v, want %+v", models, want)
	}
}

func TestMalformedEventRetainsBody(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"choices\":7}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry()})
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
	if summary.failure == nil || string(summary.failure.Body) != `{"choices":7}` {
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
		{name: "two rate limits then success", handler: rateLimitHandler(), wantRetries: 2},
		{name: "in-band rate limit", handler: inBandRateLimitHandler(), wantRetries: 1},
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
	_, _ = w.Write([]byte("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"function\":{\"name\":\"read\"}}]}}]}\n\n"))
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
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
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
	return []byte("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n")
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

type requestMessage struct {
	Role       string         `json:"role"`
	Content    jsontext.Value `json:"content"`
	ToolCallID string         `json:"tool_call_id"`
	ToolCalls  []struct {
		Type string `json:"type"`
	} `json:"tool_calls"`
}

type requestTool struct {
	Type     string `json:"type"`
	Function struct {
		Name       string         `json:"name"`
		Parameters jsontext.Value `json:"parameters"`
		Strict     bool           `json:"strict"`
	} `json:"function"`
}

func assertRequestBody(t *testing.T, body []byte) {
	t.Helper()
	var value struct {
		Messages        []requestMessage `json:"messages"`
		Tools           []requestTool    `json:"tools"`
		ReasoningEffort string           `json:"reasoning_effort"`
		StreamOptions   struct {
			IncludeUsage bool `json:"include_usage"`
		} `json:"stream_options"`
	}
	if err := json.Unmarshal(body, &value); err != nil {
		t.Fatalf("request JSON: %v\n%s", err, body)
	}
	assertHistory(t, value.Messages)
	assertRequestTools(t, value.Tools)
	if value.ReasoningEffort != "high" || !value.StreamOptions.IncludeUsage {
		t.Fatalf("reasoning = %q, stream options = %+v", value.ReasoningEffort, value.StreamOptions)
	}
}

func assertRequestTools(t *testing.T, tools []requestTool) {
	t.Helper()
	if len(tools) != 2 || tools[0].Type != "function" || tools[0].Function.Strict || tools[1].Function.Name != "apply_patch" {
		t.Fatalf("tools = %+v", tools)
	}
	if tools[1].Type != "function" || !bytes.Contains(tools[1].Function.Parameters, []byte(`"input"`)) {
		t.Fatalf("freeform tool = %+v", tools[1])
	}
}

func assertHistory(t *testing.T, messages []requestMessage) {
	t.Helper()
	if len(messages) != 4 || messages[0].Role != "system" || string(messages[0].Content) != `"system text"` {
		t.Fatalf("messages = %+v", messages)
	}
	assertToolHistory(t, messages)
}

func assertToolHistory(t *testing.T, messages []requestMessage) {
	t.Helper()
	if messages[1].Role != "assistant" || len(messages[1].ToolCalls) != 1 || messages[2].Role != "tool" || messages[2].ToolCallID != "call-1" {
		t.Fatalf("history = %+v", messages)
	}
	if string(messages[2].Content) != `"done"` || !bytes.Contains(messages[1].Content, []byte("prior")) {
		t.Fatalf("tool result or assistant text = %+v", messages)
	}
	if messages[3].Role != "user" || string(messages[3].Content) != `"after tools"` {
		t.Fatalf("user text = %+v", messages)
	}
}
