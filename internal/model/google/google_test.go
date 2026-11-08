package google

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
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(fixture)
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure != nil {
		t.Fatalf("stream error: %v", summary.failure.Err)
	}
	if summary.text != "hello" || summary.reasoning != "think" {
		t.Fatalf("text = %q, reasoning = %q", summary.text, summary.reasoning)
	}
	assertFixtureCall(t, summary.calls)
	assertFixtureUsage(t, summary)
}

func assertFixtureCall(t *testing.T, calls []model.ToolCall) {
	t.Helper()
	if len(calls) != 1 || calls[0].ID != "call-1" || string(calls[0].Args) != `{"path":"a"}` {
		t.Fatalf("calls = %+v", calls)
	}
}

func assertFixtureUsage(t *testing.T, summary streamSummary) {
	t.Helper()
	if summary.usage != (model.Usage{Input: 17, Output: 7, CacheRead: 3}) || summary.stop != "toolUse" {
		t.Fatalf("usage = %+v, stop = %q", summary.usage, summary.stop)
	}
}

func TestStreamArrayData(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: [{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}]\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure != nil || summary.text != "hello" || summary.stop != "stop" {
		t.Fatalf("summary = %+v", summary)
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
		Model:  "gemini-test",
		System: "system text",
		Messages: []model.Message{
			{Role: "assistant", Text: "prior", Reasoning: "why", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{"path":"a"}`)}}},
			{Role: "user", ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}},
		},
		Tools: []model.Tool{{
			Name:       "read",
			Parameters: jsontext.Value(`{"type":"object","properties":{"mode":{"type":"string","const":"fast"}}}`),
		}},
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
		_, _ = w.Write([]byte("data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"p\",\"name\":\"apply_patch\",\"args\":{\"input\":\"patch\"}}}]},\"finishReason\":\"STOP\"}]}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	request := model.Request{
		Model:    "gemini-test",
		Messages: []model.Message{{Role: "assistant", ToolCalls: []model.ToolCall{{ID: "p", Name: "apply_patch", Args: jsontext.Value("patch"), Freeform: true}}}},
		Tools:    []model.Tool{{Name: "apply_patch", Freeform: true}},
	}
	summary := collectStream(t, provider, request)
	if summary.failure != nil || len(summary.calls) != 1 || !summary.calls[0].Freeform || string(summary.calls[0].Args) != "patch" {
		t.Fatalf("summary = %+v", summary)
	}
	if !bytes.Contains(body, []byte(`"args":{"input":"patch"}`)) {
		t.Fatalf("freeform history = %s", body)
	}
}

func TestSchemaConstRewrite(t *testing.T) {
	t.Parallel()
	var buffer bytes.Buffer
	encoder := model.NewJSONEncoder(&buffer)
	schema := jsontext.Value(`{"type":"object","properties":{"const":{"type":"string","const":"x"},"o":{"const":{"a":1}},"z":{"const":null},"n":{"anyOf":[{"const":3}]}},"default":{"const":1}}`)
	if err := encodeSchema(encoder, schema); err != nil {
		t.Fatal(err)
	}
	want := `{"type":"object","properties":{"const":{"type":"string","enum":["x"]},"o":{"const":{"a":1}},"z":{"enum":[null]},"n":{"anyOf":[{"enum":[3]}]}},"default":{"const":1}}`
	if got := strings.TrimSpace(buffer.String()); got != want {
		t.Fatalf("schema = %s\nwant     %s", got, want)
	}
}

func TestPromptBlockFails(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"promptFeedback\":{\"blockReason\":\"SAFETY\"}}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry()})
	var retries int
	provider.cfg.Retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure == nil || summary.stop != "" || retries != 0 || !bytes.Contains(summary.failure.Body, []byte("SAFETY")) {
		t.Fatalf("summary = %+v, retries = %d", summary, retries)
	}
}

func TestSecondCandidateIgnored(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"first\"}]},\"finishReason\":\"STOP\"},{\"content\":{\"parts\":[{\"text\":\"second\"}]},\"finishReason\":\"SAFETY\"}]}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure != nil || summary.text != "first" || summary.stop != "stop" {
		t.Fatalf("summary = %+v", summary)
	}
}

func TestModels(t *testing.T) {
	t.Parallel()
	mux := http.NewServeMux()
	mux.HandleFunc("GET /v1beta/models", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Query().Get("pageToken") == "next" {
			_, _ = w.Write([]byte(`{"models":[{"name":"models/configured","supportedGenerationMethods":["generateContent"]}]}`))
			return
		}
		_, _ = w.Write([]byte(`{"models":[{"name":"models/large","inputTokenLimit":1000000,"outputTokenLimit":65536,"supportedGenerationMethods":["generateContent"]},{"name":"models/embed","supportedGenerationMethods":["embedContent"]}],"nextPageToken":"next"}`))
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, ContextWindows: map[string]int{"configured": 64000}})
	models, err := provider.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	want := []model.Model{
		{ID: "large", Context: 1000000, MaxOutput: 65536, ToolChoice: true},
		{ID: "configured", Context: 64000, ToolChoice: true},
	}
	if !reflect.DeepEqual(models, want) {
		t.Fatalf("models = %+v, want %+v", models, want)
	}
}

func TestMalformedEventRetainsBody(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"candidates\":7}\n\n"))
	}))
	t.Cleanup(server.Close)
	provider := New(Config{BaseURL: server.URL, APIKey: testKey, Retry: fastRetry()})
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure == nil || string(summary.failure.Body) != `{"candidates":7}` {
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
	_, _ = w.Write([]byte("data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"c\",\"name\":\"read\",\"args\":{}}}]}}]}\n\n"))
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
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
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
	return []byte("data: {\"candidates\":[{\"content\":{\"parts\":[]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n")
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

func assertRequestBody(t *testing.T, body []byte) {
	t.Helper()
	var value struct {
		System struct {
			Parts []struct {
				Text string `json:"text"`
			} `json:"parts"`
		} `json:"systemInstruction"`
		Contents []struct {
			Role  string `json:"role"`
			Parts []struct {
				Text         string `json:"text"`
				Thought      bool   `json:"thought"`
				FunctionCall *struct {
					ID   string         `json:"id"`
					Name string         `json:"name"`
					Args jsontext.Value `json:"args"`
				} `json:"functionCall"`
				FunctionResponse *struct {
					ID       string `json:"id"`
					Name     string `json:"name"`
					Response struct {
						Output string `json:"output"`
					} `json:"response"`
				} `json:"functionResponse"`
			} `json:"parts"`
		} `json:"contents"`
		Tools []struct {
			Declarations []struct {
				Name       string         `json:"name"`
				Parameters jsontext.Value `json:"parametersJsonSchema"`
			} `json:"functionDeclarations"`
		} `json:"tools"`
		Generation struct {
			Thinking struct {
				Include bool   `json:"includeThoughts"`
				Level   string `json:"thinkingLevel"`
			} `json:"thinkingConfig"`
		} `json:"generationConfig"`
	}
	if err := json.Unmarshal(body, &value); err != nil {
		t.Fatalf("request JSON: %v\n%s", err, body)
	}
	assertContents(t, value.Contents)
	if len(value.System.Parts) != 1 || value.System.Parts[0].Text != "system text" {
		t.Fatalf("system = %+v", value.System)
	}
	if len(value.Tools) != 1 || !bytes.Contains(value.Tools[0].Declarations[0].Parameters, []byte(`"enum":["fast"]`)) {
		t.Fatalf("tools = %+v", value.Tools)
	}
	if !value.Generation.Thinking.Include || value.Generation.Thinking.Level != "HIGH" {
		t.Fatalf("thinking = %+v", value.Generation.Thinking)
	}
}

func assertContents(t *testing.T, contents []struct {
	Role  string `json:"role"`
	Parts []struct {
		Text         string `json:"text"`
		Thought      bool   `json:"thought"`
		FunctionCall *struct {
			ID   string         `json:"id"`
			Name string         `json:"name"`
			Args jsontext.Value `json:"args"`
		} `json:"functionCall"`
		FunctionResponse *struct {
			ID       string `json:"id"`
			Name     string `json:"name"`
			Response struct {
				Output string `json:"output"`
			} `json:"response"`
		} `json:"functionResponse"`
	} `json:"parts"`
},
) {
	t.Helper()
	if len(contents) != 2 || contents[0].Role != "model" || contents[1].Role != "user" {
		t.Fatalf("contents = %+v", contents)
	}
	call := contents[0].Parts[len(contents[0].Parts)-1].FunctionCall
	response := contents[1].Parts[0].FunctionResponse
	assertFunctionCall(t, call)
	assertFunctionResponse(t, response)
}

func assertFunctionCall(t *testing.T, call *struct {
	ID   string         `json:"id"`
	Name string         `json:"name"`
	Args jsontext.Value `json:"args"`
},
) {
	t.Helper()
	if call == nil || call.Name != "read" || call.ID != "call-1" {
		t.Fatalf("call = %+v", call)
	}
}

func assertFunctionResponse(t *testing.T, response *struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Response struct {
		Output string `json:"output"`
	} `json:"response"`
},
) {
	t.Helper()
	if response == nil || response.Name != "read" || response.Response.Output != "done" {
		t.Fatalf("response = %+v", response)
	}
}
