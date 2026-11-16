package agent

import (
	"context"
	"encoding/base64"
	"encoding/json/v2"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/google"
)

const googleTextReply = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"done\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":1}}\n\n"

func TestGoogleUnlistedModelKeepsImages(t *testing.T) {
	png := []byte("\x89PNG\r\n\x1a\nabc")
	tests := []struct {
		name    string
		catalog http.Handler
	}{
		{"catalog unavailable", http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			http.Error(w, "catalog down", http.StatusInternalServerError)
		})},
		{"selected model absent", http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			w.Header().Set("Content-Type", "application/json")
			_, _ = io.WriteString(w, `{"models":[{"name":"models/gemini-other","inputTokenLimit":8192,"outputTokenLimit":2048,"supportedGenerationMethods":["generateContent"]}]}`)
		})},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			googleUnlistedModelKeepsImages(t, tc.catalog, png)
		})
	}
}

func googleUnlistedModelKeepsImages(t *testing.T, catalog http.Handler, png []byte) {
	t.Helper()
	server := newGoogleImagesServer(t, catalog)
	h := googleImagesHarness(t, server)
	thread := h.start()
	googleSubmitCompleted(t, h, thread, Input{Kind: InputPrompt, Text: "what is this", Model: "google/gemini-test", Images: []Image{{Mime: "image/png", Data: png}}})
	googleSubmitCompleted(t, h, thread, Input{Kind: InputPrompt, Text: "and now", Model: "google/gemini-test"})

	bodies := server.capturedRequests()
	if len(bodies) != 2 {
		t.Fatalf("google requests = %d, want 2", len(bodies))
	}
	wantData := base64.StdEncoding.EncodeToString(png)
	for i, body := range bodies {
		images := googleInlineData(t, body)
		if len(images) != 1 || images[0].mime != "image/png" || images[0].data != wantData {
			t.Fatalf("google request %d inlineData = %+v, want one image/png carrying the submitted bytes", i+1, images)
		}
	}
}

type googleImagesServer struct {
	srv   *httptest.Server
	mu    sync.Mutex
	posts [][]byte
}

func newGoogleImagesServer(t *testing.T, catalog http.Handler) *googleImagesServer {
	t.Helper()
	s := &googleImagesServer{}
	mux := http.NewServeMux()
	mux.Handle("GET /v1beta/models", catalog)
	mux.HandleFunc("POST /v1beta/models/gemini-test:streamGenerateContent", s.recordStream)
	s.srv = httptest.NewServer(mux)
	t.Cleanup(s.srv.Close)
	return s
}

func (s *googleImagesServer) recordStream(w http.ResponseWriter, r *http.Request) {
	body, err := io.ReadAll(r.Body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	s.mu.Lock()
	s.posts = append(s.posts, body)
	s.mu.Unlock()
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = io.WriteString(w, googleTextReply)
}

func (s *googleImagesServer) capturedRequests() [][]byte {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Clone(s.posts)
}

func googleImagesHarness(t *testing.T, server *googleImagesServer) *harness {
	return newHarness(t, func(h *harness) {
		h.cfg.Config.Model = "google/gemini-test"
		h.cfg.Providers = func(onRetry RetryFunc) (*model.Registry, error) {
			retry := model.DefaultRetryOptions()
			retry.Sleep = h.sleep
			retry.OnRetry = onRetry
			provider := google.New(google.Config{
				BaseURL: server.srv.URL,
				APIKey:  func(context.Context) (string, error) { return "test-key", nil },
				Retry:   retry,
			})
			registry := model.NewRegistry()
			return registry, registry.Register("google", provider)
		}
	})
}

func googleSubmitCompleted(t *testing.T, h *harness, thread *Thread, in Input) {
	t.Helper()
	if err := thread.Submit(t.Context(), in); err != nil {
		t.Fatalf("submit: %v", err)
	}
	h.waitIdle(thread)
	if turn := lastTurn(h.events(thread.ID())); turn.Status != statusCompleted {
		t.Fatalf("turn = %q %q, want completed", turn.Status, turn.Error)
	}
}

type googleInlineImage struct {
	mime string
	data string
}

type googleWirePart struct {
	InlineData *struct {
		MimeType string `json:"mimeType"`
		Data     string `json:"data"`
	} `json:"inlineData"`
}

type googleWireContent struct {
	Parts []googleWirePart `json:"parts"`
}

type googleWireRequest struct {
	Contents []googleWireContent `json:"contents"`
}

func googleInlineData(t *testing.T, body []byte) []googleInlineImage {
	t.Helper()
	var req googleWireRequest
	if err := json.Unmarshal(body, &req); err != nil {
		t.Fatalf("decode google request: %v", err)
	}
	var out []googleInlineImage
	for _, content := range req.Contents {
		for _, part := range content.Parts {
			if part.InlineData != nil {
				out = append(out, googleInlineImage{mime: part.InlineData.MimeType, data: part.InlineData.Data})
			}
		}
	}
	return out
}
