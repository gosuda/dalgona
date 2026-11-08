package chatgpt_test

import (
	"context"
	"encoding/json/jsontext"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync/atomic"
	"testing"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/chatgpt"
)

const codexStream = "event: response.output_text.delta\n" +
	"data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"h\"}\n\n" +
	"event: response.output_text.delta\n" +
	"data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"i\"}\n\n" +
	"event: response.completed\n" +
	"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2}}}\n\n"

type codexBody struct {
	Model        string         `json:"model"`
	Instructions string         `json:"instructions"`
	Store        bool           `json:"store"`
	Stream       bool           `json:"stream"`
	Input        jsontext.Value `json:"input"`
}

type seenRequest struct {
	path, auth, account, beta, originator string
	body                                  codexBody
}

func codexServer(t *testing.T, seen chan<- seenRequest) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, err := io.ReadAll(r.Body)
		if err != nil {
			t.Errorf("read body: %v", err)
		}
		request := seenRequest{
			path: r.URL.Path, auth: r.Header.Get("Authorization"), account: r.Header.Get("chatgpt-account-id"),
			beta: r.Header.Get("OpenAI-Beta"), originator: r.Header.Get("originator"),
		}
		if err := json.Unmarshal(raw, &request.body); err != nil {
			t.Errorf("decode body %s: %v", raw, err)
		}
		seen <- request
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, codexStream)
	}))
	t.Cleanup(server.Close)
	return server
}

func tokenServer(t *testing.T, calls *atomic.Int32) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		var body struct {
			Grant   string `json:"grant_type"`
			Refresh string `json:"refresh_token"`
		}
		if err := json.UnmarshalRead(r.Body, &body); err != nil || body.Grant != "refresh_token" || body.Refresh != "stale-refresh" {
			http.Error(w, "bad refresh", http.StatusBadRequest)
			return
		}
		_, _ = io.WriteString(w, `{"access_token":"fresh-access","refresh_token":"fresh-refresh","expires_in":3600}`)
	}))
	t.Cleanup(server.Close)
	return server
}

func drain(t *testing.T, stream model.Stream) (string, []model.Event) {
	t.Helper()
	var text string
	var events []model.Event
	for event := range stream.Events() {
		events = append(events, event)
		if delta, ok := event.(model.TextDelta); ok {
			text += delta.Text
		}
	}
	return text, events
}

func TestStreamRefreshesExpiredTokenAndSendsAccountHeaders(t *testing.T) {
	var refreshes atomic.Int32
	tokens := tokenServer(t, &refreshes)
	seen := make(chan seenRequest, 2)
	codex := codexServer(t, seen)
	provider := oauth.Providers["chatgpt"]
	provider.TokenURL = tokens.URL
	source := oauth.NewSource(provider, func(context.Context) (oauth.Token, error) {
		return oauth.Token{Access: "stale-access", Refresh: "stale-refresh", Expires: time.Now().Add(-time.Minute), Account: "acct-9"}, nil
	}, nil)
	adapter := chatgpt.New(chatgpt.Config{BaseURL: codex.URL + "/backend-api/codex", Token: source.Token})
	for range 2 {
		stream, err := adapter.Stream(t.Context(), model.Request{Model: "gpt-6.1-sol", Messages: []model.Message{{Role: "user", Text: "hello"}}})
		if err != nil {
			t.Fatal(err)
		}
		text, events := drain(t, stream)
		if text != "hi" || !slices.Contains(events, model.Event(model.Done{StopReason: "stop"})) {
			t.Fatalf("text = %q, events = %#v", text, events)
		}
		checkRequest(t, <-seen)
	}
	if refreshes.Load() != 1 {
		t.Fatalf("refreshes = %d, want 1 across two streams", refreshes.Load())
	}
}

func checkRequest(t *testing.T, request seenRequest) {
	t.Helper()
	headers := [...]string{request.path, request.auth, request.account, request.beta, request.originator}
	want := [...]string{"/backend-api/codex/responses", "Bearer fresh-access", "acct-9", "responses=experimental", "codex_cli_rs"}
	if headers != want {
		t.Fatalf("path and headers = %q, want %q", headers, want)
	}
	body := request.body
	if body.Model != "gpt-6.1-sol" || body.Instructions != "You are a helpful assistant." || body.Store || !body.Stream {
		t.Fatalf("body = %+v", body)
	}
}

func TestStreamWithoutAccountFailsBeforeSending(t *testing.T) {
	seen := make(chan seenRequest, 1)
	codex := codexServer(t, seen)
	var retries atomic.Int32
	adapter := chatgpt.New(chatgpt.Config{BaseURL: codex.URL, Token: func(context.Context) (oauth.Token, error) {
		return oauth.Token{Access: "access"}, nil
	}, Retry: model.RetryOptions{
		Sleep:   func(context.Context, time.Duration) error { return nil },
		OnRetry: func(int, model.Verdict, int, time.Duration) { retries.Add(1) },
	}})
	stream, err := adapter.Stream(t.Context(), model.Request{Model: "gpt-6-luna", System: "rules"})
	if err != nil {
		t.Fatal(err)
	}
	_, events := drain(t, stream)
	if len(events) != 1 || retries.Load() != 0 {
		t.Fatalf("events = %#v, retries = %d; want one error and no retry", events, retries.Load())
	}
	if failure, ok := events[0].(model.Error); !ok || failure.Status != http.StatusUnauthorized {
		t.Fatalf("event = %#v, want an auth model.Error", events[0])
	}
	if len(seen) != 0 {
		t.Fatal("request without an account id reached the server")
	}
}

func TestModelsUseStaticCatalogAndOverrides(t *testing.T) {
	adapter := chatgpt.New(chatgpt.Config{ContextWindows: map[string]int{"gpt-6-luna": 64000}})
	models, err := adapter.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	byID := make(map[string]model.Model, len(models))
	for _, item := range models {
		byID[item.ID] = item
	}
	if models[0].ID != "gpt-6.1-sol" || byID["gpt-6.1-terra"].Context != 500000 {
		t.Fatalf("models = %+v", models)
	}
	if byID["gpt-6-luna"].Context != 64000 || slices.Contains(byID["gpt-6-luna"].Efforts, "ultra") {
		t.Fatalf("luna = %+v", byID["gpt-6-luna"])
	}
}
