package copilot_test

import (
	"context"
	"encoding/json/jsontext"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"reflect"
	"regexp"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/copilot"
)

const (
	hubLogin    = "fake-github-login"
	chatSession = "fake-copilot-session"
	chatReply   = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n" +
		"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n" +
		"data: [DONE]\n\n"
)

var uuidV4 = regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`)

type recorded struct {
	method string
	header http.Header
}

type routes struct {
	exchange http.HandlerFunc
	chat     http.HandlerFunc
	models   http.HandlerFunc
}

type backend struct {
	*httptest.Server
	mu       sync.Mutex
	requests map[string][]recorded
}

func (b *backend) on(path string) []recorded {
	b.mu.Lock()
	defer b.mu.Unlock()
	return slices.Clone(b.requests[path])
}

func newBackend(t *testing.T, custom routes) *backend {
	t.Helper()
	b := &backend{requests: make(map[string][]recorded)}
	mux := http.NewServeMux()
	handle := func(method, path string, handler http.HandlerFunc) {
		mux.HandleFunc(method+" "+path, func(w http.ResponseWriter, r *http.Request) {
			b.mu.Lock()
			b.requests[path] = append(b.requests[path], recorded{method: r.Method, header: r.Header.Clone()})
			b.mu.Unlock()
			handler(w, r)
		})
	}
	handle(http.MethodGet, "/token", handlerOr(custom.exchange, grantSession))
	handle(http.MethodPost, "/chat/completions", handlerOr(custom.chat, streamReply))
	handle(http.MethodGet, "/models", handlerOr(custom.models, emptyCatalog))
	b.Server = httptest.NewServer(mux)
	t.Cleanup(b.Close)
	return b
}

func handlerOr(custom, fallback http.HandlerFunc) http.HandlerFunc {
	if custom != nil {
		return custom
	}
	return fallback
}

func grantSession(w http.ResponseWriter, _ *http.Request) {
	fmt.Fprintf(w, `{"token":%q,"expires_at":%d}`, chatSession, time.Now().Add(time.Hour).Unix())
}

func streamReply(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = io.WriteString(w, chatReply)
}

func emptyCatalog(w http.ResponseWriter, _ *http.Request) {
	_, _ = io.WriteString(w, `{"data":[]}`)
}

func refuse(status int, body string) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(status)
		_, _ = io.WriteString(w, body)
	}
}

func newProvider(b *backend, retries *atomic.Int32) *copilot.Provider {
	return copilot.New(copilot.Config{
		Token:    func(context.Context) (string, error) { return hubLogin, nil },
		BaseURL:  b.URL,
		TokenURL: b.URL + "/token",
		Retry: model.RetryOptions{
			Sleep:   func(context.Context, time.Duration) error { return nil },
			Random:  func() float64 { return 0.5 },
			OnRetry: func(int, model.Verdict, int, time.Duration) { retries.Add(1) },
		},
	})
}

func drain(stream model.Stream) (string, []model.Event) {
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

func ask(t *testing.T, provider model.Provider, messages []model.Message) (string, []model.Event) {
	t.Helper()
	stream, err := provider.Stream(t.Context(), model.Request{Model: "gpt-4.1", Messages: messages})
	if err != nil {
		t.Fatal(err)
	}
	return drain(stream)
}

func identityHeaders(header http.Header) (string, [5]string) {
	editor := header.Get("Editor-Version")
	return editor, [...]string{
		header.Get("Authorization"), header.Get("Editor-Plugin-Version"), header.Get("Copilot-Integration-Id"),
		header.Get("Openai-Intent"), header.Get("X-Initiator"),
	}
}

func requireIdentity(t *testing.T, name string, header http.Header, initiator string) {
	t.Helper()
	editor, got := identityHeaders(header)
	want := [...]string{"Bearer " + chatSession, editor, "vscode-chat", "conversation-panel", initiator}
	if got != want || !strings.HasPrefix(editor, "dalgona/") {
		t.Fatalf("%s: headers = %q, editor = %q, want %q", name, got, editor, want)
	}
}

func TestCopilotHeaders(t *testing.T) {
	b := newBackend(t, routes{})
	var retries atomic.Int32
	provider := newProvider(b, &retries)
	history := []model.Message{
		{Role: "user", Text: "list the files"},
		{Role: "assistant", ToolCalls: []model.ToolCall{{ID: "call-1", Name: "exec", Args: jsontext.Value(`{"command":"ls"}`)}}},
		{Role: "user", ToolResults: []model.ToolResult{{ID: "call-1", Name: "exec", Output: jsontext.Value(`"a.txt"`)}}},
	}
	cases := []struct {
		name      string
		messages  []model.Message
		initiator string
	}{
		{"first prompt", history[:1], "user"},
		{"after a tool result", history, "agent"},
		{"follow-up after the tool turn", append(slices.Clone(history), model.Message{Role: "assistant", Text: "done"}, model.Message{Role: "user", Text: "thanks"}), "user"},
	}
	for _, c := range cases {
		if text, _ := ask(t, provider, c.messages); text != "hi" {
			t.Fatalf("%s: text = %q, want hi", c.name, text)
		}
	}
	chats := b.on("/chat/completions")
	if len(chats) != len(cases) {
		t.Fatalf("chat requests = %d, want %d", len(chats), len(cases))
	}
	seenIDs := map[string]bool{}
	for index, request := range chats {
		requireIdentity(t, cases[index].name, request.header, cases[index].initiator)
		id := request.header.Get("X-Request-Id")
		if !uuidV4.MatchString(id) || seenIDs[id] {
			t.Fatalf("%s: X-Request-Id = %q, want a fresh UUID v4", cases[index].name, id)
		}
		seenIDs[id] = true
	}
	requireOneExchange(t, b)
}

func requireOneExchange(t *testing.T, b *backend) {
	t.Helper()
	exchanges := b.on("/token")
	if len(exchanges) != 1 || exchanges[0].method != http.MethodGet || exchanges[0].header.Get("Authorization") != "Token "+hubLogin {
		t.Fatalf("exchanges = %+v, want one GET with the GitHub credential", exchanges)
	}
}

const (
	rejected = "copilot: token exchange rejected the credential (HTTP 401); inspect the account before attempting login again"
	refused  = "copilot: token exchange refused (HTTP %d); check account access and service availability"
)

func requireRefusal(t *testing.T, status int, body, want string, hidden ...string) {
	t.Helper()
	b := newBackend(t, routes{exchange: refuse(status, body)})
	var retries atomic.Int32
	_, events := ask(t, newProvider(b, &retries), []model.Message{{Role: "user", Text: "hi"}})
	failure, ok := model.Error{}, false
	if len(events) == 1 {
		failure, ok = events[0].(model.Error)
	}
	if !ok || failure.Message != want {
		t.Fatalf("events = %#v, want one error %q", events, want)
	}
	for _, secret := range hidden {
		if strings.Contains(failure.Message, secret) {
			t.Fatalf("error prints %q: %q", secret, failure.Message)
		}
	}
	if exchanges, chats := len(b.on("/token")), len(b.on("/chat/completions")); exchanges != 1 || chats != 0 || retries.Load() != 0 {
		t.Fatalf("exchanges = %d, chats = %d, retries = %d; want 1, 0, 0", exchanges, chats, retries.Load())
	}
}

func TestCopilotExchangeRefusals(t *testing.T) {
	scanned := "ghp_" + strings.Repeat("x", 36)
	cases := []struct {
		name   string
		status int
		body   string
		want   string
	}{
		{"unauthorized", 401, `{"message":"Bad credentials","documentation_url":"https://docs.github.com/rest"}`, rejected + `; provider message: "Bad credentials"`},
		{"forbidden", 403, `{"message":"Resource not accessible"}`, fmt.Sprintf(refused, 403) + `; provider message: "Resource not accessible"`},
		{"not found without a body", 404, ``, fmt.Sprintf(refused, 404)},
		{"html body is not echoed", 401, `<html><body>Unicorn</body></html>`, rejected},
		{"other status", 500, `{"message":"boom"}`, `copilot: token exchange failed (HTTP 500); provider message: "boom"`},
		{"echoed account credential", 401, `{"message":"token ` + hubLogin + ` is revoked"}`, rejected + `; provider message: "token [redacted] is revoked"`},
		{"credential pattern", 401, `{"message":"bad ` + scanned + `"}`, rejected + `; provider message: "bad [redacted]"`},
		{"control characters", 401, `{"message":"bad\u001b[0m\n\tcredentials"}`, rejected + `; provider message: "bad[0m credentials"`},
		{"long message", 403, `{"message":"` + strings.Repeat("a", 250) + `"}`, fmt.Sprintf(refused, 403) + `; provider message: "` + strings.Repeat("a", 200) + `…"`},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { requireRefusal(t, c.status, c.body, c.want, hubLogin, scanned) })
	}
}

func TestCopilotChatRateLimitRetriesWithTheCachedSession(t *testing.T) {
	var chats atomic.Int32
	b := newBackend(t, routes{chat: func(w http.ResponseWriter, r *http.Request) {
		if chats.Add(1) == 1 {
			refuse(http.StatusTooManyRequests, `{"error":{"code":"rate_limited"}}`)(w, r)
			return
		}
		streamReply(w, r)
	}})
	var retries atomic.Int32
	text, _ := ask(t, newProvider(b, &retries), []model.Message{{Role: "user", Text: "hi"}})
	if text != "hi" || retries.Load() != 1 || len(b.on("/chat/completions")) != 2 || len(b.on("/token")) != 1 {
		t.Fatalf("text = %q, retries = %d, chats = %d, exchanges = %d; want hi, 1, 2, 1",
			text, retries.Load(), len(b.on("/chat/completions")), len(b.on("/token")))
	}
}

func TestCopilotWithoutLoginSendsNothing(t *testing.T) {
	b := newBackend(t, routes{})
	provider := copilot.New(copilot.Config{BaseURL: b.URL, TokenURL: b.URL + "/token"})
	const want = "copilot: not logged in; run /login copilot"
	_, events := ask(t, provider, []model.Message{{Role: "user", Text: "hi"}})
	if failure, ok := events[0].(model.Error); !ok || len(events) != 1 || failure.Message != want {
		t.Fatalf("events = %#v, want one error %q", events, want)
	}
	if _, err := provider.Models(t.Context()); err == nil || err.Error() != want {
		t.Fatalf("Models error = %v, want %q", err, want)
	}
	if len(b.on("/token"))+len(b.on("/chat/completions"))+len(b.on("/models")) != 0 {
		t.Fatal("a request reached the server without a GitHub credential")
	}
}

const pickerCatalog = `{"data":[
 {"id":"gpt-4.1","model_picker_enabled":true,"capabilities":{"limits":{"max_context_window_tokens":128000,"max_output_tokens":16384},"supports":{"reasoning_effort":["Low","medium","high","medium"," "]}}},
 {"id":"text-embedding-3-small","model_picker_enabled":false,"capabilities":{"limits":{"max_context_window_tokens":8191}}},
 {"id":"claude-sonnet-4","model_picker_enabled":true,"capabilities":{"limits":{"max_context_window_tokens":200000,"max_output_tokens":64000},"supports":{"reasoning_effort":true}}},
 {"id":"gpt-4.1","model_picker_enabled":true,"capabilities":{"limits":{"max_context_window_tokens":1}}},
 {"id":"no-limits","model_picker_enabled":true},
 {"id":"pinned","model_picker_enabled":true,"capabilities":{"limits":{"max_context_window_tokens":999999}}}
]}`

func TestCopilotModelsFiltersPicker(t *testing.T) {
	b := newBackend(t, routes{models: func(w http.ResponseWriter, _ *http.Request) { _, _ = io.WriteString(w, pickerCatalog) }})
	provider := copilot.New(copilot.Config{
		Token: func(context.Context) (string, error) { return hubLogin, nil }, BaseURL: b.URL, TokenURL: b.URL + "/token",
		ContextWindows: map[string]int{"pinned": 64000},
	})
	models, err := provider.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	want := []model.Model{
		{ID: "gpt-4.1", Context: 128000, MaxOutput: 16384, Efforts: []string{"low", "medium", "high"}, ToolChoice: true},
		{ID: "claude-sonnet-4", Context: 200000, MaxOutput: 64000, ToolChoice: true},
		{ID: "no-limits", Context: 128000, ToolChoice: true},
		{ID: "pinned", Context: 64000, ToolChoice: true},
	}
	if !reflect.DeepEqual(models, want) {
		t.Fatalf("models = %+v, want %+v", models, want)
	}
	requests := b.on("/models")
	if len(requests) != 1 || requests[0].method != http.MethodGet {
		t.Fatalf("model requests = %+v, want one GET", requests)
	}
	requireIdentity(t, "models", requests[0].header, "user")
}

func TestCopilotModelsErrors(t *testing.T) {
	cases := []struct {
		name  string
		serve routes
		want  string
	}{
		{"catalog refusal", routes{models: refuse(500, `{"message":"catalog down"}`)}, `copilot: models request returned HTTP 500; provider message: "catalog down"`},
		{"echoed session credential", routes{models: refuse(401, `{"message":"bad `+chatSession+`"}`)}, `copilot: models request returned HTTP 401; provider message: "bad [redacted]"`},
		{"exchange refusal", routes{exchange: refuse(401, ``)}, rejected},
		{"malformed catalog", routes{models: func(w http.ResponseWriter, _ *http.Request) { _, _ = io.WriteString(w, `{"data":7}`) }}, `copilot: decode models: `},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			var retries atomic.Int32
			_, err := newProvider(newBackend(t, c.serve), &retries).Models(t.Context())
			if err == nil || !strings.HasPrefix(err.Error(), c.want) {
				t.Fatalf("Models error = %v, want prefix %q", err, c.want)
			}
		})
	}
}
