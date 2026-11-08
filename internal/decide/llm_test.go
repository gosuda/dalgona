package decide_test

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/model/chatcompat"
)

func TestLLMAskDecodesDirectAnswers(t *testing.T) {
	t.Parallel()
	requests := make(chan capturedRequest, 1)
	mux := http.NewServeMux()
	mux.HandleFunc("POST /chat/completions", func(w http.ResponseWriter, r *http.Request) {
		captured, err := captureRequest(r)
		if err != nil {
			http.Error(w, "read request", http.StatusInternalServerError)
			return
		}
		requests <- captured
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"{\\\"team\\\":\\\"backend\\\",\\\"is_bug\\\":0.96,\\\"urgency\\\":1.5}\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":4}}\n\ndata: [DONE]\n\n")
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	provider := newChatProvider(server.URL, "test-token")
	answers, err := decide.NewLLM(provider, "test-smol").Ask(t.Context(), jsontext.Value(`{"ticket":"Checkout fails"}`), testQuestions())
	if err != nil {
		t.Fatal(err)
	}
	checkChatRequest(t, <-requests)
	checkLLMAnswers(t, answers)
}

func TestLLMAskRejectsInvalidAnswers(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name    string
		content string
	}{
		{name: "malformed JSON", content: "not-json"},
		{name: "extra answer", content: `{"team":"backend","is_bug":0.9,"urgency":1,"other":1}`},
		{name: "wrong value type", content: `{"team":"backend","is_bug":"yes","urgency":1}`},
		{name: "score above criteria range", content: `{"team":"backend","is_bug":0.9,"urgency":3}`},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			server := newChatServer(t, test.content)
			provider := newChatProvider(server.URL, "token")
			_, err := decide.NewLLM(provider, "test-smol").Ask(t.Context(), jsontext.Value(`{"ticket":"x"}`), testQuestions())
			if err == nil {
				t.Fatal("Ask succeeded")
			}
			if !strings.Contains(err.Error(), "llm: invalid JSON") {
				t.Fatalf("Ask error = %v", err)
			}
		})
	}
}

func TestLLMAskHonorsConfiguredTimeout(t *testing.T) {
	t.Parallel()
	release := make(chan struct{})
	mux := http.NewServeMux()
	mux.HandleFunc("POST /chat/completions", func(_ http.ResponseWriter, r *http.Request) {
		select {
		case <-r.Context().Done():
		case <-release:
		}
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	t.Cleanup(func() { close(release) })
	provider := newChatProvider(server.URL, "token")
	_, err := decide.NewLLMWithTimeout(provider, "test-smol", 25*time.Millisecond).Ask(t.Context(), jsontext.Value(`{"ticket":"x"}`), map[string]decide.Question{"is_bug": {Type: decide.TypeNoul}})
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("Ask error = %v", err)
	}
}

func newChatProvider(baseURL, token string) *chatcompat.Provider {
	return chatcompat.New(chatcompat.Config{
		BaseURL: baseURL,
		APIKey:  func(context.Context) (string, error) { return token, nil },
		Retry:   model.DefaultRetryOptions(),
	})
}

func newChatServer(t *testing.T, content string) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("POST /chat/completions", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":"+quoted(content)+"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
	})
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

func checkChatRequest(t *testing.T, captured capturedRequest) {
	t.Helper()
	if got := captured.header.Get("Authorization"); got != "Bearer test-token" {
		t.Errorf("Authorization = %q", got)
	}
	request := decodeObject(t, captured.bodyReader())
	if got := decodeString(t, request["model"]); got != "test-smol" {
		t.Errorf("model = %q", got)
	}
	messages := decodeArray(t, request["messages"])
	if len(messages) != 2 {
		t.Fatalf("message count = %d", len(messages))
	}
	checkChatSystem(t, messages[0])
	user := nestedObject(t, messages[1])
	if got := decodeString(t, user["role"]); got != "user" {
		t.Errorf("user role = %q", got)
	}
	checkStateAndQuestions(t, jsontext.Value(decodeString(t, user["content"])))
}

func checkChatSystem(t *testing.T, raw jsontext.Value) {
	t.Helper()
	system := nestedObject(t, raw)
	if got := decodeString(t, system["role"]); got != "system" {
		t.Errorf("system role = %q", got)
	}
	if prompt := decodeString(t, system["content"]); !strings.Contains(prompt, "Return only one JSON object") {
		t.Errorf("system prompt = %q", prompt)
	}
	if prompt := decodeString(t, system["content"]); !strings.Contains(prompt, "len(criteria)-1") {
		t.Errorf("system prompt has no score scale = %q", prompt)
	}
}

func checkLLMAnswers(t *testing.T, answers map[string]decide.Answer) {
	t.Helper()
	if len(answers) != 3 || answers["team"].Choice != "backend" {
		t.Fatalf("answers = %+v", answers)
	}
	checkNoul(t, answers["is_bug"], 0.96)
	checkScore(t, answers["urgency"], 1.5)
	if answers["team"].Probabilities != nil || answers["team"].Confidence != 0 {
		t.Errorf("LLM answer invented metadata: %+v", answers["team"])
	}
}

func quoted(value string) string {
	var out strings.Builder
	out.Grow(len(value) + 2)
	out.WriteByte('"')
	for _, r := range value {
		if r == '"' || r == '\\' {
			out.WriteByte('\\')
		}
		out.WriteRune(r)
	}
	out.WriteByte('"')
	return out.String()
}
