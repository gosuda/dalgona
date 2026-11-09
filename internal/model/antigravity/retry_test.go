package antigravity_test

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/antigravity"
)

const arrayStream = "data: [{\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"he\"}]}}]},\"traceId\":\"t1\"}," +
	"{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"llo\"}]},\"finishReason\":\"STOP\"}]," +
	"\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":3,\"cachedContentTokenCount\":4,\"thoughtsTokenCount\":1}}]\n\n"

func newSingleReplyServer(t *testing.T, handle func(w http.ResponseWriter, r *http.Request)) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(handle))
	t.Cleanup(server.Close)
	return server
}

func TestStreamDecodesArrayEvents(t *testing.T) {
	server := newSingleReplyServer(t, func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v1internal:loadCodeAssist":
			_, _ = io.WriteString(w, paidProject(false))
		case "/v1internal:streamGenerateContent":
			w.Header().Set("Content-Type", "text/event-stream")
			_, _ = io.WriteString(w, arrayStream)
		default:
			http.NotFound(w, r)
		}
	})
	adapter := antigravity.New(antigravity.Config{
		BaseURL: server.URL,
		Token:   func(context.Context) (string, error) { return "access", nil },
	})
	streamHello(t, adapter, model.Request{Model: "gemini-3.8-flash", Messages: []model.Message{{Role: "user", Text: "hi"}}})
}

func newForbiddenDiscoveryServer(t *testing.T, attempts *atomic.Int32) *httptest.Server {
	t.Helper()
	return newSingleReplyServer(t, func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v1internal:loadCodeAssist":
			http.Error(w, `{"error":{"code":403,"message":"PERMISSION_DENIED"}}`, http.StatusForbidden)
		case "/v1internal:streamGenerateContent":
			attempts.Add(1)
			http.Error(w, "unreachable", http.StatusInternalServerError)
		default:
			http.NotFound(w, r)
		}
	})
}

func collectFailure(events []model.Event) model.Error {
	var failure model.Error
	for _, event := range events {
		if value, ok := event.(model.Error); ok {
			failure = value
		}
	}
	return failure
}

func TestStreamAuthOnForbiddenDiscovery(t *testing.T) {
	var attempts, onRetry atomic.Int32
	server := newForbiddenDiscoveryServer(t, &attempts)
	adapter := antigravity.New(antigravity.Config{
		BaseURL: server.URL,
		Token:   func(context.Context) (string, error) { return "access", nil },
		Retry: model.RetryOptions{
			StartTimeout: time.Second,
			Sleep:        func(context.Context, time.Duration) error { return nil },
			OnRetry:      func(int, model.Verdict, int, time.Duration) { onRetry.Add(1) },
		},
	})
	stream, err := adapter.Stream(t.Context(), model.Request{Model: "gemini-3.8-flash", Messages: []model.Message{{Role: "user", Text: "hi"}}})
	if err != nil {
		t.Fatal(err)
	}
	_, _, events := collect(t, stream)
	if retries := onRetry.Load(); retries != 0 {
		t.Fatalf("retries = %d, want 0 for auth failure", retries)
	}
	if streamAttempts := attempts.Load(); streamAttempts != 0 {
		t.Fatalf("stream attempts = %d, want 0", streamAttempts)
	}
	failure := collectFailure(events)
	if failure.Status != http.StatusForbidden || !strings.Contains(failure.Message, "403") {
		t.Fatalf("failure = %+v, want status 403 with the provider body", failure)
	}
}
