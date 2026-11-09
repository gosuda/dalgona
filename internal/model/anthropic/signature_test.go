package anthropic

import (
	"bytes"
	"encoding/json/jsontext"
	"io"
	"net/http"
	"os"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/model"
)

func TestStreamCapturesSignatureDelta(t *testing.T) {
	t.Parallel()
	fixture, err := os.ReadFile("testdata/basic.sse")
	if err != nil {
		t.Fatal(err)
	}
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write(fixture)
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "claude-test", Reasoning: "high"})
	if summary.failure != nil {
		t.Fatalf("stream error: %v", summary.failure.Err)
	}
	assertFixtureCall(t, summary.calls)
	if summary.calls[0].Signature != "sig" {
		t.Fatalf("signature = %q, want sig", summary.calls[0].Signature)
	}
}

func TestEncodeReplaysThinkingBlockBeforeToolUse(t *testing.T) {
	t.Parallel()
	var body []byte
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		_, _ = w.Write(successFixture())
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	request := model.Request{
		Model: "claude-test",
		Messages: []model.Message{
			{Role: "assistant", Reasoning: "why", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{"path":"a"}`), Signature: "sig-9"}}},
			{Role: "user", ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}},
		},
		Tools:     []model.Tool{{Name: "read", Parameters: jsontext.Value(`{"type":"object"}`)}},
		Reasoning: "high",
	}
	_ = collectStream(t, provider, request)
	want := `{"type":"thinking","thinking":"why","signature":"sig-9"}`
	if !bytes.Contains(body, []byte(want)) {
		t.Fatalf("request body %s misses %s", body, want)
	}
	if strings.Contains(string(body), `{"type":"text","text":"why"}`) {
		t.Fatalf("request body %s replays reasoning as visible text", body)
	}
	if bytes.Index(body, []byte(want)) > bytes.Index(body, []byte(`"type":"tool_use"`)) {
		t.Fatalf("request body %s puts the thinking block after tool_use", body)
	}
}

func TestEncodeDropsUnsignedReasoning(t *testing.T) {
	t.Parallel()
	var body []byte
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		_, _ = w.Write(successFixture())
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	request := model.Request{
		Model: "claude-test",
		Messages: []model.Message{
			{Role: "assistant", Reasoning: "why", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{}`)}}},
			{Role: "user", ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}},
		},
		Reasoning: "high",
	}
	_ = collectStream(t, provider, request)
	if strings.Contains(string(body), "why") {
		t.Fatalf("request body %s replays reasoning without its signature", body)
	}
}

func TestEncodeOmitsThinkingBlockWhenDisabled(t *testing.T) {
	t.Parallel()
	var body []byte
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		_, _ = w.Write(successFixture())
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	request := model.Request{
		Model: "claude-test",
		Messages: []model.Message{
			{Role: "assistant", Reasoning: "why", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{}`), Signature: "sig-9"}}},
			{Role: "user", ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}},
		},
	}
	_ = collectStream(t, provider, request)
	if strings.Contains(string(body), `"signature"`) || strings.Contains(string(body), "why") {
		t.Fatalf("request body %s sends thinking state with thinking disabled", body)
	}
}
