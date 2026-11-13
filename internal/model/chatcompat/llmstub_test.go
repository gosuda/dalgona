package chatcompat

import (
	"context"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/llmstub"
	"github.com/gosuda/dalgona/internal/model"
)

const stubKey = "test"

func newStubServer(t *testing.T, src string) *httptest.Server {
	t.Helper()
	script, err := llmstub.Parse("chatcompat", []byte(src))
	if err != nil {
		t.Fatal(err)
	}
	server := httptest.NewServer(llmstub.NewHandler(script, llmstub.Options{Key: stubKey}))
	t.Cleanup(server.Close)
	return server
}

func stubRequest() model.Request {
	return model.Request{Model: "qwen-test", Messages: []model.Message{{Role: "user", Text: "hello"}}}
}

func TestLLMStubTurnDecodes(t *testing.T) {
	t.Parallel()
	server := newStubServer(t, `{"models":[{"id":"qwen-test"}],"turns":[{"id":"t1","when":{"user":"hello"},"steps":[`+
		`{"reasoning":"think"},`+
		`{"text":"hel"},`+
		`{"tool_call":{"id":"call-1","name":"read","arguments":{"path":"a"},"chunks":3}},`+
		`{"tool_call":{"id":"call-2","name":"exec","arguments":{"command":"pwd"}}},`+
		`{"finish":"tool_calls","usage":{"prompt_tokens":5,"completion_tokens":3}}]}]}`)
	provider := New(Config{BaseURL: server.URL + "/v1", APIKey: testKey})
	assertDecodedTurn(t, collectStream(t, provider, stubRequest()))
}

func assertDecodedTurn(t *testing.T, summary streamSummary) {
	t.Helper()
	if summary.failure != nil {
		t.Fatalf("stream error: %v", summary.failure.Err)
	}
	if summary.text != "hel" || summary.reasoning != "think" {
		t.Fatalf("text = %q, reasoning = %q", summary.text, summary.reasoning)
	}
	assertDecodedCalls(t, summary.calls)
	if summary.usage != (model.Usage{Input: 5, Output: 3}) || summary.stop != "toolUse" {
		t.Fatalf("usage = %+v, stop = %q", summary.usage, summary.stop)
	}
}

func assertDecodedCalls(t *testing.T, calls []model.ToolCall) {
	t.Helper()
	if len(calls) != 2 {
		t.Fatalf("calls = %+v", calls)
	}
	if first := calls[0]; first.ID != "call-1" || first.Name != "read" || string(first.Args) != `{"path":"a"}` {
		t.Fatalf("first call = %+v", first)
	}
	if second := calls[1]; second.ID != "call-2" || second.Name != "exec" || string(second.Args) != `{"command":"pwd"}` {
		t.Fatalf("second call = %+v", second)
	}
}

func TestLLMStubRetryAfterReplacesDelay(t *testing.T) {
	t.Parallel()
	server := newStubServer(t, `{"models":[{"id":"qwen-test"}],"turns":[`+
		`{"id":"limited","when":{"user":"hello"},"once":true,"status":429,"retry_after":"2","body":{"error":{"code":"rate_limit","message":"slow down"}}},`+
		`{"id":"ok","when":{"user":"hello"},"steps":[{"text":"ok"},{"finish":"stop"}]}]}`)
	retry := fastRetry()
	var slept time.Duration
	var sleeps int
	retry.Sleep = func(_ context.Context, d time.Duration) error {
		slept += d
		sleeps++
		return nil
	}
	provider := New(Config{BaseURL: server.URL + "/v1", APIKey: testKey, Retry: retry})
	summary := collectStream(t, provider, stubRequest())
	if summary.failure != nil || summary.text != "ok" {
		t.Fatalf("summary = %+v", summary)
	}
	if slept != 2*time.Second || sleeps == 0 {
		t.Fatalf("slept %v in %d steps, want 2s", slept, sleeps)
	}
}

func TestLLMStubErrorFrameFailsWithoutRetry(t *testing.T) {
	t.Parallel()
	server := newStubServer(t, `{"models":[{"id":"qwen-test"}],"turns":[{"id":"t1","when":{"user":"hello"},"steps":[`+
		`{"text":"partial"},`+
		`{"error_frame":{"code":"server_error","message":"upstream reset"}}]}]}`)
	retry := fastRetry()
	retries := 0
	retry.OnRetry = func(int, model.Verdict, int, time.Duration) { retries++ }
	provider := New(Config{BaseURL: server.URL + "/v1", APIKey: testKey, Retry: retry})
	summary := collectStream(t, provider, stubRequest())
	if summary.failure == nil {
		t.Fatal("stream without error")
	}
	if retries != 0 {
		t.Fatalf("retries = %d, want 0", retries)
	}
}
