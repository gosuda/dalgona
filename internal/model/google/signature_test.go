package google

import (
	"bytes"
	"encoding/json/jsontext"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/model"
)

func TestStreamCarriesThoughtSignature(t *testing.T) {
	t.Parallel()
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"plan\"},{\"functionCall\":{\"id\":\"call-1\",\"name\":\"read\",\"args\":{\"path\":\"a\"}},\"thoughtSignature\":\"sig-1\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n"))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure != nil {
		t.Fatalf("stream error: %v", summary.failure.Err)
	}
	assertFixtureCall(t, summary.calls)
	if summary.calls[0].Signature != "sig-1" {
		t.Fatalf("signature = %q, want sig-1", summary.calls[0].Signature)
	}
}

func TestStreamThoughtSignatureOnTextPart(t *testing.T) {
	t.Parallel()
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"think\",\"thought\":true,\"thoughtSignature\":\"sig-t\"},{\"functionCall\":{\"id\":\"call-1\",\"name\":\"read\",\"args\":{}}}]},\"finishReason\":\"STOP\"}]}\n\n"))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure != nil {
		t.Fatalf("stream error: %v", summary.failure.Err)
	}
	if summary.reasoning != "think" {
		t.Fatalf("reasoning = %q, want think", summary.reasoning)
	}
	if summary.calls[0].Signature != "" {
		t.Fatalf("signature = %q, want empty for a call part without one", summary.calls[0].Signature)
	}
}

func TestEncodeReplaysThoughtSignature(t *testing.T) {
	t.Parallel()
	var body []byte
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		_, _ = w.Write(successFixture())
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	request := model.Request{
		Model: "gemini-test",
		Messages: []model.Message{
			{Role: "assistant", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{"path":"a"}`), Signature: "sig-1"}}},
			{Role: "user", ToolResults: []model.ToolResult{{ID: "call-1", Output: jsontext.Value(`"done"`)}}},
		},
		Tools: []model.Tool{{Name: "read", Parameters: jsontext.Value(`{"type":"object"}`)}},
	}
	_ = collectStream(t, provider, request)
	want := `"functionCall":{"id":"call-1","name":"read","args":{"path":"a"}},"thoughtSignature":"sig-1"`
	if !bytes.Contains(body, []byte(want)) {
		t.Fatalf("request body %s misses %s", body, want)
	}
}

func TestEncodeOmitsAbsentThoughtSignature(t *testing.T) {
	t.Parallel()
	var body []byte
	server := newStreamServer(t, func(w http.ResponseWriter, r *http.Request) {
		body, _ = io.ReadAll(r.Body)
		_, _ = w.Write(successFixture())
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	request := model.Request{
		Model:    "gemini-test",
		Messages: []model.Message{{Role: "assistant", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "read", Args: jsontext.Value(`{}`)}}}},
	}
	_ = collectStream(t, provider, request)
	if strings.Contains(string(body), "thoughtSignature") {
		t.Fatalf("request body %s carries a signature it never received", body)
	}
}
