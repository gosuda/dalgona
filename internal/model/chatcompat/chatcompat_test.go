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
	"strings"
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
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(successFixture())
	})
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
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"p\",\"function\":{\"name\":\"apply_patch\",\"arguments\":\"{\\\"input\\\":\\\"patch\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"))
	})
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
			t.Parallel()
			server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
				w.Header().Set("Content-Type", "text/event-stream")
				_, _ = w.Write([]byte(test.body))
			})
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
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte(body))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
	want := model.Usage{Input: 15, Output: 7, CacheRead: 4, CacheWrite: 1}
	if summary.failure != nil || summary.usage != want {
		t.Fatalf("summary = %+v, want usage %+v", summary, want)
	}
}

func TestReasoningPrecedenceSkipsNonStrings(t *testing.T) {
	t.Parallel()
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"\",\"reasoning\":{\"thinking\":\"nested\"},\"reasoning_text\":\"fallback\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
	if summary.failure != nil || summary.reasoning != "fallback" {
		t.Fatalf("summary = %+v", summary)
	}
}

func TestRawJSONToolArguments(t *testing.T) {
	t.Parallel()
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"r\",\"function\":{\"name\":\"read\",\"arguments\":{\"path\":\"a\", \"limit\":2}}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n"))
	})
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
	server := newProviderOnlyServer(t, http.MethodGet, "/models", mux)
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
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"choices\":7}\n\n"))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry()})
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
	if summary.failure == nil || string(summary.failure.Body) != `{"choices":7}` {
		t.Fatalf("failure = %+v", summary.failure)
	}
}

type retryScenario struct {
	name         string
	handler      http.HandlerFunc
	startTimeout time.Duration
	wantRetries  int
	wantError    bool
}

func TestRetryContract(t *testing.T) {
	t.Parallel()
	scenarios := []retryScenario{
		{name: "two rate limits then success", handler: rateLimitHandler(), wantRetries: 2},
		{name: "in-band rate limit", handler: inBandRateLimitHandler(), wantRetries: 1},
		{name: "bad request", handler: badRequestHandler, wantError: true},
		{name: "tool call then drop", handler: toolDropHandler, wantError: true},
		{name: "headers then silence", handler: silentHandler, startTimeout: 100 * time.Millisecond, wantRetries: 3, wantError: true},
	}
	for _, scenario := range scenarios {
		t.Run(scenario.name, func(t *testing.T) {
			t.Parallel()
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

func runRetryScenario(t *testing.T, scenario retryScenario) {
	t.Helper()
	server := newStreamServer(t, scenario.handler)
	retry := fastRetry()
	retry.StartTimeout = scenario.startTimeout
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: retry})
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	summary := collectStream(t, provider, model.Request{Model: "test-model"})
	if retries != scenario.wantRetries || (summary.failure != nil) != scenario.wantError {
		t.Fatalf("retries = %d, failure = %+v; want %d, error %v", retries, summary.failure, scenario.wantRetries, scenario.wantError)
	}
}

func testKey(context.Context) (string, error) { return "test", nil }

func newStreamServer(t *testing.T, handler http.HandlerFunc) *httptest.Server {
	t.Helper()
	return newProviderOnlyServer(t, http.MethodPost, "/chat/completions", handler)
}

func newProviderOnlyServer(t *testing.T, method, path string, handler http.Handler) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != method || r.URL.Path != path || r.Header.Get("Authorization") != "Bearer test" {
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

func TestEncodeRequestSendsImageURL(t *testing.T) {
	t.Parallel()
	body, err := encodeRequest(model.Request{Messages: []model.Message{{
		Role:   "user",
		Text:   "What is this?",
		Images: []model.Image{{Mime: "image/png", Data: []byte("\x89PNG\r\n\x1a\n")}},
	}}})
	if err != nil {
		t.Fatal(err)
	}
	var value struct {
		Messages []any `json:"messages"`
	}
	if err := json.Unmarshal(body, &value); err != nil {
		t.Fatalf("request JSON: %v\n%s", err, body)
	}
	const want = `{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,iVBORw0KGgo="}},{"type":"text","text":"What is this?"}]}`
	var expected any
	if err := json.Unmarshal([]byte(want), &expected); err != nil {
		t.Fatal(err)
	}
	if len(value.Messages) != 1 || !reflect.DeepEqual(value.Messages[0], expected) {
		t.Fatalf("messages = %v, want %s", value.Messages, want)
	}
}

func TestEncodeRequestSendsImageAfterToolResults(t *testing.T) {
	t.Parallel()
	body, err := encodeRequest(model.Request{Messages: []model.Message{{
		Role:        "user",
		Text:        "look",
		ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}},
		Images:      []model.Image{{Mime: "image/gif", Data: []byte("GIF89a")}},
	}}})
	if err != nil {
		t.Fatal(err)
	}
	var value struct {
		Messages []requestMessage `json:"messages"`
	}
	if err := json.Unmarshal(body, &value); err != nil {
		t.Fatalf("request JSON: %v\n%s", err, body)
	}
	if len(value.Messages) != 2 || value.Messages[0].Role != "tool" || value.Messages[1].Role != "user" {
		t.Fatalf("messages = %+v", value.Messages)
	}
	if !bytes.Contains(value.Messages[1].Content, []byte(`"image_url"`)) || !bytes.Contains(value.Messages[1].Content, []byte(`data:image/gif;base64,R0lGODlh`)) {
		t.Fatalf("user content = %s", value.Messages[1].Content)
	}
}

func TestDecodeModelsReadsInputModalities(t *testing.T) {
	t.Parallel()
	models, err := decodeModels(strings.NewReader(`{"data":[{"id":"a/text-only","architecture":{"input_modalities":["text"]}},{"id":"a/vision","architecture":{"input_modalities":["text","image"]}},{"id":"b/plain"}]}`), nil)
	if err != nil {
		t.Fatal(err)
	}
	var got []bool
	for _, m := range models {
		got = append(got, m.Images)
	}
	if !reflect.DeepEqual(got, []bool{false, true, false}) {
		t.Fatalf("images = %v, want [false true false]", got)
	}
}
