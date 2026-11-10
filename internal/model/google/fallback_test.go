package google

import (
	"net/http"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/model"
)

func TestFallbackIDsStayUniqueAcrossRequests(t *testing.T) {
	t.Parallel()
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"exec\",\"args\":{}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n"))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	first := singleCall(t, provider)
	second := singleCall(t, provider)
	if first.ID == "" || second.ID == "" {
		t.Fatalf("empty fallback ids = %+v %+v", first, second)
	}
	if first.ID == second.ID {
		t.Fatalf("fallback ids collide = %q", first.ID)
	}
	if !strings.Contains(first.ID, "exec") || !strings.Contains(second.ID, "exec") {
		t.Fatalf("fallback ids lose tool name = %+v %+v", first, second)
	}
}

func singleCall(t *testing.T, provider *Provider) model.ToolCall {
	t.Helper()
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure != nil || len(summary.calls) != 1 {
		t.Fatalf("summary = %+v", summary)
	}
	return summary.calls[0]
}

func TestProviderSuppliedIDStaysUnchanged(t *testing.T) {
	t.Parallel()
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"call-1\",\"name\":\"read\",\"args\":{}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n"))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure != nil || len(summary.calls) != 1 {
		t.Fatalf("summary = %+v", summary)
	}
	if summary.calls[0].ID != "call-1" {
		t.Fatalf("provider id changed = %q", summary.calls[0].ID)
	}
}

func TestRepeatedProviderIDGetsFallback(t *testing.T) {
	t.Parallel()
	server := newStreamServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = w.Write([]byte("data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"call-1\",\"name\":\"read\",\"args\":{}}},{\"functionCall\":{\"id\":\"call-1\",\"name\":\"read\",\"args\":{}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":1,\"candidatesTokenCount\":1}}\n\n"))
	})
	provider := New(Config{BaseURL: server.URL, APIKey: testKey})
	summary := collectStream(t, provider, model.Request{Model: "gemini-test"})
	if summary.failure != nil || len(summary.calls) != 2 {
		t.Fatalf("summary = %+v", summary)
	}
	if summary.calls[0].ID != "call-1" {
		t.Fatalf("first provider id changed = %+v", summary.calls)
	}
	if summary.calls[1].ID == "call-1" {
		t.Fatalf("repeated provider id kept = %+v", summary.calls)
	}
	if !strings.Contains(summary.calls[1].ID, "read") {
		t.Fatalf("fallback id loses tool name = %+v", summary.calls)
	}
}
