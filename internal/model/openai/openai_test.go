package openai

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"reflect"
	"slices"
	"sync/atomic"
	"testing"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/model"
)

type requestTool struct {
	Type       string         `json:"type"`
	Name       string         `json:"name"`
	Strict     bool           `json:"strict"`
	Parameters jsontext.Value `json:"parameters"`
}

type requestReasoning struct {
	Effort  string `json:"effort"`
	Summary string `json:"summary"`
}

type streamSummary struct {
	text      string
	reasoning string
	calls     []model.ToolCall
	usage     model.Usage
	stop      string
}

func TestStreamFixture(t *testing.T) {
	t.Parallel()
	fixture, err := os.ReadFile("testdata/basic.sse")
	if err != nil {
		t.Fatal(err)
	}
	server := fixtureServer(fixture)
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := drainStream(t, provider, model.Request{Model: "gpt-test"})
	if summary.text != "hello" || summary.reasoning != "think" {
		t.Fatalf("text = %q, reasoning = %q", summary.text, summary.reasoning)
	}
	assertFixtureCalls(t, summary.calls)
	assertFixtureUsage(t, summary)
}

func assertFixtureCalls(t *testing.T, calls []model.ToolCall) {
	t.Helper()
	if len(calls) != 2 {
		t.Fatalf("tool calls = %d, want 2", len(calls))
	}
	if calls[0].ID != "call-1" || calls[0].Name != "read" || string(calls[0].Args) != `{"path":"a"}` {
		t.Fatalf("first call = %+v", calls[0])
	}
	if calls[1].ID != "call-2" || calls[1].Name != "apply_patch" || !calls[1].Freeform || string(calls[1].Args) != "*** Patch" {
		t.Fatalf("second call = %+v", calls[1])
	}
}

func assertFixtureUsage(t *testing.T, summary streamSummary) {
	t.Helper()
	if summary.usage != (model.Usage{Input: 15, Output: 7, CacheRead: 3, CacheWrite: 2}) {
		t.Fatalf("usage = %+v", summary.usage)
	}
	if summary.stop != "toolUse" {
		t.Fatalf("stop = %q, want toolUse", summary.stop)
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
		Model:  "gpt-test",
		System: "system text",
		Messages: []model.Message{{
			Role:      "assistant",
			Text:      "prior",
			Reasoning: "why",
			ToolCalls: []model.ToolCall{
				{ID: "call-1", Name: "read", Args: jsontext.Value(`{"path":"a"}`)},
				{ID: "call-2", Name: "apply_patch", Args: jsontext.Value("patch"), Freeform: true},
			},
		}, {
			Role: "user",
			ToolResults: []model.ToolResult{
				{ID: "call-1", Output: jsontext.Value(`"done"`)},
				{ID: "call-2", Name: "apply_patch", Output: jsontext.Value(`"patched"`)},
			},
		}},
		Tools: []model.Tool{
			{Name: "read", Description: "Read", Parameters: jsontext.Value(`{"type":"object"}`)},
			{Name: "apply_patch", Freeform: true},
		},
		Reasoning: "high",
	}
	_ = drainStream(t, provider, request)
	assertRequestBody(t, body)
}

func TestBaseURLFromEnvironment(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(successFixture())
	}))
	t.Cleanup(server.Close)
	t.Setenv("OPENAI_BASE_URL", server.URL)
	provider := New(Config{APIKey: testKey})
	if summary := drainStream(t, provider, model.Request{Model: "gpt-test"}); summary.stop != "stop" {
		t.Fatalf("stop = %q", summary.stop)
	}
}

func TestRetryAfterTwoRateLimits(t *testing.T) {
	t.Parallel()
	var requests atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if requests.Add(1) <= 2 {
			w.WriteHeader(http.StatusTooManyRequests)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(successFixture())
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(nil)})
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	summary := drainStream(t, provider, model.Request{Model: "gpt-test"})
	if retries != 2 || summary.stop != "stop" {
		t.Fatalf("retries = %d, stop = %q", retries, summary.stop)
	}
}

func TestRetryInBandRateLimit(t *testing.T) {
	t.Parallel()
	var requests atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		if requests.Add(1) == 1 {
			_, _ = w.Write([]byte("{\"error\":{\"code\":429,\"message\":\"slow down\"}}\n"))
			return
		}
		_, _ = w.Write(successFixture())
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(nil)})
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	_ = drainStream(t, provider, model.Request{Model: "gpt-test"})
	if retries != 1 {
		t.Fatalf("retries = %d, want 1", retries)
	}
}

func TestRetryAfterOverLimitStopsTheRequest(t *testing.T) {
	t.Parallel()
	var requests atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		w.Header().Set("Retry-After", "120")
		w.WriteHeader(http.StatusTooManyRequests)
	}))
	t.Cleanup(server.Close)
	var retries int
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(func(int, model.Verdict, int, time.Duration) { retries++ })})
	stream, err := provider.Stream(t.Context(), model.Request{Model: "gpt-test"})
	if err != nil {
		t.Fatal(err)
	}
	failure, ok := lastEvent(t, stream).(model.Error)
	if !ok {
		t.Fatal("last event is not model.Error")
	}
	if failure.Message != "rate limited for 120 s, over the 60 s wait limit" || failure.Status != http.StatusTooManyRequests || retries != 0 || requests.Load() != 1 {
		t.Fatalf("message = %q, status = %d, retries = %d, requests = %d", failure.Message, failure.Status, retries, requests.Load())
	}
}

func TestQuotaRateLimitDoesNotRetry(t *testing.T) {
	t.Parallel()
	var requests atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		w.WriteHeader(http.StatusTooManyRequests)
		_, _ = w.Write([]byte(`{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}`))
	}))
	t.Cleanup(server.Close)
	var retries int
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(func(int, model.Verdict, int, time.Duration) { retries++ })})
	stream, err := provider.Stream(t.Context(), model.Request{Model: "gpt-test"})
	if err != nil {
		t.Fatal(err)
	}
	failure, ok := lastEvent(t, stream).(model.Error)
	if !ok {
		t.Fatal("last event is not model.Error")
	}
	if failure.Status != http.StatusTooManyRequests || retries != 0 || requests.Load() != 1 {
		t.Fatalf("status = %d, retries = %d, requests = %d; want 429, 0, 1", failure.Status, retries, requests.Load())
	}
}

func TestBadRequestDoesNotRetry(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusBadRequest)
		_, _ = w.Write([]byte(`{"error":{"message":"bad"}}`))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(nil)})
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	stream, err := provider.Stream(t.Context(), model.Request{Model: "gpt-test"})
	if err != nil {
		t.Fatal(err)
	}
	event := lastEvent(t, stream)
	failure, ok := event.(model.Error)
	if !ok || failure.Status != http.StatusBadRequest || retries != 0 {
		t.Fatalf("event = %#v, retries = %d", event, retries)
	}
}

func TestToolCallThenDropDoesNotRetry(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"function_call\",\"call_id\":\"c\",\"name\":\"read\"}}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(nil)})
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	stream, err := provider.Stream(t.Context(), model.Request{Model: "gpt-test"})
	if err != nil {
		t.Fatal(err)
	}
	event := lastEvent(t, stream)
	if _, ok := event.(model.Error); !ok || retries != 0 {
		t.Fatalf("event = %#v, retries = %d", event, retries)
	}
}

func TestStartTimeout(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		if flusher, ok := w.(http.Flusher); ok {
			flusher.Flush()
		}
		<-r.Context().Done()
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(nil)})
	provider.cfg.Retry.StartTimeout = 100 * time.Millisecond
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	stream, err := provider.Stream(t.Context(), model.Request{Model: "gpt-test"})
	if err != nil {
		t.Fatal(err)
	}
	start := time.Now()
	event := lastEvent(t, stream)
	failure, ok := event.(model.Error)
	if !ok || retries != 3 {
		t.Fatalf("event = %#v, retries = %d, want 3 retries", event, retries)
	}
	if time.Since(start) < 300*time.Millisecond {
		t.Fatalf("stream failed before four start deadlines: %v", failure.Err)
	}
}

func TestRefusalFailsWithoutRetry(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"type\":\"response.refusal.delta\",\"delta\":\"no request timeout\"}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(nil)})
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	stream, err := provider.Stream(t.Context(), model.Request{Model: "gpt-test"})
	if err != nil {
		t.Fatal(err)
	}
	event := lastEvent(t, stream)
	failure, ok := event.(model.Error)
	if !ok || failure.Err.Error() != "openai: refusal" || !bytes.Contains(failure.Body, []byte("refusal")) || retries != 0 {
		t.Fatalf("event = %#v, retries = %d", event, retries)
	}
}

func TestMalformedEventRetainsBody(t *testing.T) {
	t.Parallel()
	server := fixtureServer([]byte("data: {\"type\":\"response.output_text.delta\",\"delta\":7}\n\n"))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(nil)})
	stream, err := provider.Stream(t.Context(), model.Request{Model: "gpt-test"})
	if err != nil {
		t.Fatal(err)
	}
	failure, ok := lastEvent(t, stream).(model.Error)
	if !ok || string(failure.Body) != `{"type":"response.output_text.delta","delta":7}` {
		t.Fatalf("failure = %#v", failure)
	}
}

func TestResponseDoneEndsStreamWithoutWaitingForEOF(t *testing.T) {
	t.Parallel()
	const hold = 5 * time.Second
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"ok\"}\n\n" +
			"event: response.done\n" +
			"data: {\"type\":\"response.done\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n"))
		w.(http.Flusher).Flush()
		select {
		case <-r.Context().Done():
		case <-time.After(hold):
		}
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry(nil)})
	start := time.Now()
	summary := drainStream(t, provider, model.Request{Model: "gpt-test"})
	if elapsed := time.Since(start); elapsed >= hold {
		t.Fatalf("stream waited %v for EOF after response.done", elapsed)
	}
	if summary.text != "ok" || summary.stop != "stop" || summary.usage.Input != 5 || summary.usage.Output != 2 {
		t.Fatalf("summary = %+v", summary)
	}
}

func TestModels(t *testing.T) {
	t.Parallel()
	mux := http.NewServeMux()
	mux.HandleFunc("GET /models", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(`{"data":[{"id":"known"},{"id":"other"}]}`))
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, ContextWindows: map[string]int{"known": 64000}})
	models, err := provider.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	want := []model.Model{{ID: "known", Context: 64000, ToolChoice: true}, {ID: "other", Context: 128000, ToolChoice: true}}
	if !reflect.DeepEqual(models, want) {
		t.Fatalf("models = %+v, want %+v", models, want)
	}
}

func fixtureServer(fixture []byte) *httptest.Server {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(fixture)
	}))
	return server
}

func testKey(context.Context) (string, error) {
	return "test", nil
}

func fastRetry(onRetry func(int, model.Verdict, int, time.Duration)) model.RetryOptions {
	return model.RetryOptions{
		StartTimeout: time.Second,
		Sleep:        func(context.Context, time.Duration) error { return nil },
		Random:       func() float64 { return 0.5 },
		OnRetry:      onRetry,
	}
}

func successFixture() []byte {
	return []byte("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n")
}

func drainStream(t *testing.T, provider *Provider, request model.Request) streamSummary {
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
			t.Fatalf("stream error: %v status=%d body=%s", value.Err, value.Status, value.Body)
		}
	}
	return summary
}

func lastEvent(t *testing.T, stream model.Stream) model.Event {
	t.Helper()
	var event model.Event
	for item := range stream.Events() {
		event = item
	}
	if event == nil {
		t.Fatal("stream ended without an event")
	}
	return event
}

type requestItem struct {
	Type      string `json:"type"`
	Role      string `json:"role"`
	CallID    string `json:"call_id"`
	Name      string `json:"name"`
	Arguments string `json:"arguments"`
	Input     string `json:"input"`
	Output    string `json:"output"`
}

func assertRequestBody(t *testing.T, body []byte) {
	t.Helper()
	var value struct {
		Model        string           `json:"model"`
		Instructions string           `json:"instructions"`
		Store        bool             `json:"store"`
		Input        []requestItem    `json:"input"`
		Tools        []requestTool    `json:"tools"`
		Reasoning    requestReasoning `json:"reasoning"`
	}
	if err := json.Unmarshal(body, &value); err != nil {
		t.Fatalf("request JSON: %v\n%s", err, body)
	}
	if value.Model != "gpt-test" || value.Instructions != "system text" || value.Store {
		t.Fatalf("request header = %+v", value)
	}
	assertHistory(t, value.Input)
	assertRequestTools(t, value.Tools)
	assertRequestReasoning(t, body, value.Reasoning.Effort, value.Reasoning.Summary)
}

func assertHistory(t *testing.T, items []requestItem) {
	t.Helper()
	want := []requestItem{
		{Type: "message", Role: "assistant"},
		{Type: "function_call", CallID: "call-1", Name: "read", Arguments: `{"path":"a"}`},
		{Type: "custom_tool_call", CallID: "call-2", Name: "apply_patch", Input: "patch"},
		{Type: "function_call_output", CallID: "call-1", Output: "done"},
		{Type: "custom_tool_call_output", CallID: "call-2", Output: "patched"},
	}
	if !slices.Equal(items, want) {
		t.Fatalf("input items = %+v, want %+v", items, want)
	}
}

func assertRequestTools(t *testing.T, tools []requestTool) {
	t.Helper()
	if len(tools) != 2 || tools[0].Type != "function" || tools[0].Name != "read" || tools[0].Strict {
		t.Fatalf("function tool = %+v", tools)
	}
	if tools[1].Type != "custom" || tools[1].Name != "apply_patch" || len(tools[1].Parameters) != 0 {
		t.Fatalf("custom tool = %+v", tools[1])
	}
}

func assertRequestReasoning(t *testing.T, body []byte, effort, summary string) {
	t.Helper()
	if effort != "high" || summary != "auto" || !bytes.Contains(body, []byte(`"include":["reasoning.encrypted_content"]`)) {
		t.Fatalf("reasoning effort = %q, summary = %q", effort, summary)
	}
}
