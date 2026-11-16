package anthropic

import (
	"encoding/json/jsontext"
	"io"
	"net/http"
	"os"
	"reflect"
	"strings"
	"testing"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dal/model"
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

type signatureContentBlock struct {
	Type      string `json:"type"`
	Thinking  string `json:"thinking"`
	Signature string `json:"signature"`
	Data      string `json:"data"`
	Text      string `json:"text"`
}

type signatureRequestMessage struct {
	Role    string                  `json:"role"`
	Content []signatureContentBlock `json:"content"`
}

type signatureRequestPayload struct {
	Messages []signatureRequestMessage `json:"messages"`
}

func decodeSignatureRequest(t *testing.T, body []byte) signatureRequestPayload {
	t.Helper()
	var payload signatureRequestPayload
	if err := json.Unmarshal(body, &payload); err != nil {
		t.Fatalf("request JSON: %v\n%s", err, body)
	}
	return payload
}

func assistantContent(t *testing.T, payload signatureRequestPayload) []signatureContentBlock {
	t.Helper()
	for _, message := range payload.Messages {
		if message.Role == "assistant" {
			return message.Content
		}
	}
	t.Fatalf("request has no assistant message: %+v", payload.Messages)
	return nil
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
	summary := collectStream(t, provider, request)
	if summary.failure != nil {
		t.Fatalf("stream error: %v", summary.failure.Err)
	}
	content := assistantContent(t, decodeSignatureRequest(t, body))
	if len(content) != 2 {
		t.Fatalf("assistant content = %+v, want [thinking, tool_use]", content)
	}
	if content[0].Type != "thinking" || content[0].Thinking != "why" || content[0].Signature != "sig-9" {
		t.Fatalf("first block = %+v, want thinking block with signature sig-9", content[0])
	}
	if content[1].Type != "tool_use" {
		t.Fatalf("second block = %+v, want tool_use", content[1])
	}
	assertReasoningNotVisible(t, content, body)
}

func assertReasoningNotVisible(t *testing.T, content []signatureContentBlock, body []byte) {
	t.Helper()
	for _, block := range content {
		if block.Type == "text" && block.Text == "why" {
			t.Fatalf("request body %s replays reasoning as visible text", body)
		}
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

func TestStreamKeepsRedactedThinking(t *testing.T) {
	t.Parallel()
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"enc-1\"}}\n\n" +
			"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n" +
			"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"read\",\"input\":{}}}\n\n" +
			"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a\\\"}\"}}\n\n" +
			"event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n" +
			"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n" +
			"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "claude-test", Reasoning: "high"})
	if summary.failure != nil {
		t.Fatalf("stream error: %v", summary.failure.Err)
	}
	if len(summary.calls) != 1 || !reflect.DeepEqual(summary.calls[0].RedactedThinking, []string{"enc-1"}) {
		t.Fatalf("calls = %+v, want redacted thinking enc-1", summary.calls)
	}
}

func TestEncodeReplaysRedactedThinkingUnchanged(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name      string
		message   model.Message
		wantTypes []string
	}{
		{
			name:      "redacted only",
			message:   model.Message{Role: "assistant", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{"path":"a"}`), RedactedThinking: []string{"enc-1"}}}},
			wantTypes: []string{"redacted_thinking", "tool_use"},
		},
		{
			name:      "signed thinking first",
			message:   model.Message{Role: "assistant", Reasoning: "why", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{"path":"a"}`), Signature: "sig-9", RedactedThinking: []string{"enc-1"}}}},
			wantTypes: []string{"thinking", "redacted_thinking", "tool_use"},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
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
					tc.message,
					{Role: "user", ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}},
				},
				Tools:     []model.Tool{{Name: "read", Parameters: jsontext.Value(`{"type":"object"}`)}},
				Reasoning: "high",
			}
			summary := collectStream(t, provider, request)
			if summary.failure != nil {
				t.Fatalf("stream error: %v", summary.failure.Err)
			}
			content := assistantContent(t, decodeSignatureRequest(t, body))
			assertRedactedReplayContent(t, content, tc.wantTypes)
		})
	}
}

func assertRedactedReplayContent(t *testing.T, content []signatureContentBlock, wantTypes []string) {
	t.Helper()
	if len(content) != len(wantTypes) {
		t.Fatalf("assistant content = %+v, want %d blocks", content, len(wantTypes))
	}
	redactedCount := 0
	for i, block := range content {
		if block.Type != wantTypes[i] {
			t.Fatalf("assistant content[%d] = %q, want %q; full content = %+v", i, block.Type, wantTypes[i], content)
		}
		if block.Type == "redacted_thinking" {
			redactedCount++
		}
		assertReplayedThinkingFields(t, block)
	}
	if redactedCount != 1 {
		t.Fatalf("redacted_thinking count = %d, want 1", redactedCount)
	}
}

func assertReplayedThinkingFields(t *testing.T, block signatureContentBlock) {
	t.Helper()
	if block.Type == "redacted_thinking" {
		if block.Data != "enc-1" {
			t.Fatalf("redacted_thinking data = %q, want enc-1", block.Data)
		}
	}
	if block.Type == "thinking" {
		if block.Thinking != "why" || block.Signature != "sig-9" {
			t.Fatalf("thinking block = %+v, want thinking=why signature=sig-9", block)
		}
	}
}

func TestEncodeOmitsRedactedThinkingWhenDisabled(t *testing.T) {
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
			{Role: "assistant", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{}`), RedactedThinking: []string{"enc-1"}}}},
			{Role: "user", ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}},
		},
	}
	_ = collectStream(t, provider, request)
	if strings.Contains(string(body), "redacted_thinking") {
		t.Fatalf("request body %s replays redacted thinking with thinking disabled", body)
	}
}
