package antigravity_test

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/antigravity"
)

const replyStream = "data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"he\"}]}}]},\"traceId\":\"t1\"}\n\n" +
	"data: {\"response\":{\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"llo\"}]},\"finishReason\":\"STOP\"}]," +
	"\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":3,\"cachedContentTokenCount\":4,\"thoughtsTokenCount\":1}}}\n\n"

type envelope struct {
	Project   string `json:"project"`
	RequestID string `json:"requestId"`
	Model     string `json:"model"`
	UserAgent string `json:"userAgent"`
	Request   struct {
		Contents []struct {
			Role  string `json:"role"`
			Parts []struct {
				Text string `json:"text"`
			} `json:"parts"`
		} `json:"contents"`
	} `json:"request"`
}

type cloudCode struct {
	t          *testing.T
	mu         sync.Mutex
	loads      int
	onboarded  bool
	polls      int
	streams    []envelope
	auth       []string
	agents     []string
	loadReply  func(onboarded bool) string
	streamCode atomic.Int32
}

func (c *cloudCode) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.auth = append(c.auth, r.Header.Get("Authorization"))
	c.agents = append(c.agents, r.Header.Get("User-Agent"))
	switch r.URL.Path {
	case "/v1internal:loadCodeAssist":
		c.loads++
		_, _ = io.WriteString(w, c.loadReply(c.onboarded))
	case "/v1internal:onboardUser":
		_, _ = io.WriteString(w, `{"name":"operations/op-1","done":false}`)
	case "/v1internal/operations/op-1":
		c.polls++
		c.onboarded = true
		_, _ = io.WriteString(w, `{"name":"operations/op-1","done":true,"response":{"@type":"t","cloudaicompanionProject":"proj-new"}}`)
	case "/v1internal:streamGenerateContent":
		c.serveStream(w, r)
	default:
		http.NotFound(w, r)
	}
}

func (c *cloudCode) serveStream(w http.ResponseWriter, r *http.Request) {
	if code := c.streamCode.Swap(0); code != 0 {
		http.Error(w, `{"error":{"code":503,"status":"UNAVAILABLE"}}`, int(code))
		return
	}
	var body envelope
	if err := json.UnmarshalRead(r.Body, &body); err != nil {
		c.t.Errorf("decode envelope: %v", err)
	}
	c.streams = append(c.streams, body)
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = io.WriteString(w, replyStream)
}

func paidProject(bool) string {
	return `{"currentTier":{"id":"standard-tier"},"paidTier":{"id":"g1-pro"},"allowedTiers":[{"id":"free-tier"}],"cloudaicompanionProject":"proj-7"}`
}

func newCloudCode(t *testing.T, reply func(bool) string) (*cloudCode, *httptest.Server) {
	t.Helper()
	backend := &cloudCode{t: t, loadReply: reply}
	server := httptest.NewServer(backend)
	t.Cleanup(server.Close)
	return backend, server
}

func refreshingSource(t *testing.T, refreshes *atomic.Int32) *oauth.Source {
	t.Helper()
	tokens := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		refreshes.Add(1)
		if err := r.ParseForm(); err != nil || r.PostForm.Get("refresh_token") != "stale-refresh" || r.PostForm.Get("client_secret") == "" {
			http.Error(w, "bad refresh", http.StatusBadRequest)
			return
		}
		_, _ = io.WriteString(w, `{"access_token":"fresh-access","expires_in":3600}`)
	}))
	t.Cleanup(tokens.Close)
	provider := oauth.Providers["antigravity"]
	provider.TokenURL = tokens.URL
	return oauth.NewSource(provider, func(context.Context) (oauth.Token, error) {
		return oauth.Token{Access: "stale-access", Refresh: "stale-refresh", Expires: time.Now().Add(-time.Second)}, nil
	}, nil)
}

func collect(t *testing.T, stream model.Stream) (string, model.Usage, []model.Event) {
	t.Helper()
	var text strings.Builder
	var usage model.Usage
	var events []model.Event
	for event := range stream.Events() {
		events = append(events, event)
		switch value := event.(type) {
		case model.TextDelta:
			text.WriteString(value.Text)
		case model.Usage:
			usage = value
		}
	}
	return text.String(), usage, events
}

func TestStreamDiscoversProjectOnceAndUnwrapsResponses(t *testing.T) {
	var refreshes atomic.Int32
	backend, server := newCloudCode(t, paidProject)
	adapter := antigravity.New(antigravity.Config{BaseURL: server.URL, Token: refreshingSource(t, &refreshes).Access})
	request := model.Request{Model: "gemini-3.8-flash", Reasoning: "high", Messages: []model.Message{{Role: "user", Text: "hi"}}}
	for range 2 {
		streamHello(t, adapter, request)
	}
	backend.mu.Lock()
	defer backend.mu.Unlock()
	if backend.loads != 2 || refreshes.Load() != 1 {
		t.Fatalf("loadCodeAssist calls = %d, refreshes = %d; want 2 and 1", backend.loads, refreshes.Load())
	}
	checkEnvelope(t, backend.streams[1])
	for index := range backend.auth {
		if backend.auth[index] != "Bearer fresh-access" || !strings.HasPrefix(backend.agents[index], "antigravity/hub/") {
			t.Fatalf("request %d auth = %q, agent = %q", index, backend.auth[index], backend.agents[index])
		}
	}
}

func streamHello(t *testing.T, adapter *antigravity.Provider, request model.Request) {
	t.Helper()
	stream, err := adapter.Stream(t.Context(), request)
	if err != nil {
		t.Fatal(err)
	}
	text, usage, events := collect(t, stream)
	if text != "hello" || usage != (model.Usage{Input: 6, Output: 4, CacheRead: 4}) {
		t.Fatalf("text = %q, usage = %+v, events = %#v", text, usage, events)
	}
	if done, ok := events[len(events)-1].(model.Done); !ok || done.StopReason != "stop" {
		t.Fatalf("last event = %#v", events[len(events)-1])
	}
}

func checkEnvelope(t *testing.T, body envelope) {
	t.Helper()
	if body.Project != "proj-7" || body.Model != "gemini-3.8-flash-high" || body.UserAgent != "antigravity" {
		t.Fatalf("envelope = %+v", body)
	}
	if !strings.HasPrefix(body.RequestID, "agent/") || !strings.HasSuffix(body.RequestID, "/1") || strings.Count(body.RequestID, "/") != 4 {
		t.Fatalf("requestId = %q", body.RequestID)
	}
	contents, err := json.Marshal(body.Request.Contents)
	if err != nil || string(contents) != `[{"role":"user","parts":[{"text":"hi"}]}]` {
		t.Fatalf("contents = %s, err = %v", contents, err)
	}
}

func TestProjectOnboardsFreeTierAfterOnePoll(t *testing.T) {
	backend, server := newCloudCode(t, func(onboarded bool) string {
		if !onboarded {
			return `{"allowedTiers":[{"id":"free-tier"}]}`
		}
		return `{"currentTier":{"id":"free-tier"},"cloudaicompanionProject":"proj-new"}`
	})
	adapter := antigravity.New(antigravity.Config{BaseURL: server.URL, Token: func(context.Context) (string, error) { return "access", nil }})
	project, err := adapter.Project(t.Context())
	if err != nil || project != "proj-new" {
		t.Fatalf("project = %q, err = %v", project, err)
	}
	backend.mu.Lock()
	defer backend.mu.Unlock()
	if backend.polls != 1 {
		t.Fatalf("operation polls = %d, want 1", backend.polls)
	}
}

func TestProjectReportsFreeTierIneligibility(t *testing.T) {
	_, server := newCloudCode(t, func(bool) string {
		return `{"ineligibleTiers":[{"tierId":"free-tier","reasonMessage":"Verify your account","validationUrl":"https://accounts.google.com/verify"}]}`
	})
	adapter := antigravity.New(antigravity.Config{BaseURL: server.URL, Token: func(context.Context) (string, error) { return "access", nil }})
	_, err := adapter.Project(t.Context())
	if err == nil || !strings.Contains(err.Error(), "Verify your account\nhttps://accounts.google.com/verify") {
		t.Fatalf("err = %v", err)
	}
}

func TestStreamRetriesUnavailable(t *testing.T) {
	backend, server := newCloudCode(t, paidProject)
	backend.streamCode.Store(http.StatusServiceUnavailable)
	var retries atomic.Int32
	adapter := antigravity.New(antigravity.Config{
		BaseURL: server.URL, Project: "proj-cfg",
		Token: func(context.Context) (string, error) { return "access", nil },
		Retry: model.RetryOptions{
			StartTimeout: time.Second,
			Sleep:        func(context.Context, time.Duration) error { return nil },
			OnRetry:      func(int, model.Verdict, int, time.Duration) { retries.Add(1) },
		},
	})
	stream, err := adapter.Stream(t.Context(), model.Request{Model: "gemini-3.5-flash-lite", Messages: []model.Message{{Role: "user", Text: "hi"}}})
	if err != nil {
		t.Fatal(err)
	}
	text, _, events := collect(t, stream)
	if text != "hello" || retries.Load() != 1 {
		t.Fatalf("text = %q, retries = %d, events = %#v", text, retries.Load(), events)
	}
	backend.mu.Lock()
	defer backend.mu.Unlock()
	if backend.loads != 0 || backend.streams[0].Project != "proj-cfg" || backend.streams[0].Model != "gemini-3.5-flash-lite" {
		t.Fatalf("loads = %d, envelope = %+v", backend.loads, backend.streams[0])
	}
}

func TestModelsCarryCatalogLimits(t *testing.T) {
	adapter := antigravity.New(antigravity.Config{ContextWindows: map[string]int{"gemini-3-pro": 200000}})
	models, err := adapter.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	for _, item := range models {
		if item.ID == "gemini-3-pro" && (item.Context != 200000 || item.MaxOutput != 65535 || len(item.Efforts) != 2) {
			t.Fatalf("gemini-3-pro = %+v", item)
		}
		if item.ID == "gemini-4-argon" && item.Context != 1048576 {
			t.Fatalf("gemini-4-argon = %+v", item)
		}
	}
}
