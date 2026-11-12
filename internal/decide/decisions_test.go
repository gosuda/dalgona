package decide_test

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/decide"
)

func TestDecisionsAskDecodesTypedAnswers(t *testing.T) {
	t.Parallel()
	requests := make(chan capturedRequest, 1)
	server := newDecisionsServer(t, "/api/alpha/decisions", func(w http.ResponseWriter, r *http.Request) {
		captured, err := captureRequest(r)
		if err != nil {
			http.Error(w, "read request", http.StatusInternalServerError)
			return
		}
		requests <- captured
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"id":"gen-test","model":"typesafe/jev-1.14","answers":{"team":{"type":"choice","choice":"backend","probabilities":{"backend":0.84,"frontend":0.16},"confidence":0.75},"is_bug":{"type":"noul","noul":0.96},"urgency":{"type":"score","score":1.99,"probabilities":{"0":0,"1":0.01,"2":0.99},"confidence":0.99}},"usage":{"input_tokens":20,"output_tokens":3}}`)
	})
	client := decide.NewDecisions(decide.DecisionsConfig{
		BaseURL: server.URL + "/api/alpha/decisions",
		Model:   "typesafe/jev-1.14",
		Token: func(context.Context) (string, error) {
			return "test-token", nil
		},
	})
	answers, err := client.Ask(t.Context(), jsontext.Value(`{"ticket":"Checkout fails"}`), testQuestions())
	if err != nil {
		t.Fatal(err)
	}
	checkDecisionsRequest(t, <-requests)
	checkDecisionsAnswers(t, answers)
}

func TestDecisionsAskRejectsEmptyModel(t *testing.T) {
	t.Parallel()
	var requests atomic.Int32
	server := newDecisionsServer(t, "/decisions", func(_ http.ResponseWriter, _ *http.Request) {
		requests.Add(1)
	})
	client := decide.NewDecisions(decide.DecisionsConfig{
		BaseURL: server.URL + "/decisions",
		Model:   "",
		Token: func(context.Context) (string, error) {
			return "t", nil
		},
	})
	_, err := client.Ask(t.Context(), jsontext.Value(`"x"`), map[string]decide.Question{
		"is_bug": {Type: decide.TypeNoul},
	})
	if err == nil || err.Error() != "decisions: model identifier is required" {
		t.Fatalf("error = %v, want decisions: model identifier is required", err)
	}
	if got := requests.Load(); got != 0 {
		t.Fatalf("requests sent = %d, want 0", got)
	}
}

func TestDecisionsAskRejectsInvalidResponses(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		body string
	}{
		{name: "malformed JSON", body: `{"answers":`},
		{name: "missing answer", body: `{"answers":{}}`},
		{name: "wrong answer type", body: `{"answers":{"is_bug":{"type":"choice","choice":"yes"}}}`},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			server := newDecisionsServer(t, "/", func(w http.ResponseWriter, _ *http.Request) {
				_, _ = io.WriteString(w, test.body)
			})
			client := decide.NewDecisions(decide.DecisionsConfig{
				BaseURL: server.URL,
				Model:   "typesafe/jev-1.14",
				Token:   func(context.Context) (string, error) { return "token", nil },
			})
			questions := map[string]decide.Question{"is_bug": {Type: decide.TypeNoul}}
			if _, err := client.Ask(t.Context(), jsontext.Value(`{"ticket":"x"}`), questions); err == nil {
				t.Fatal("Ask succeeded")
			}
		})
	}
}

func TestDecisionsAskRejectsInvalidQuestions(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name     string
		question decide.Question
	}{
		{name: "choice scalar criteria", question: decide.Question{Type: decide.TypeChoice, Criteria: "backend"}},
		{name: "noul scalar criteria", question: decide.Question{Type: decide.TypeNoul, Criteria: "yes"}},
		{name: "empty score criteria", question: decide.Question{Type: decide.TypeScore, Criteria: []string{}}},
		{name: "score scalar criteria", question: decide.Question{Type: decide.TypeScore, Criteria: "high"}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			var requests atomic.Int32
			server := newDecisionsServer(t, "/", func(w http.ResponseWriter, _ *http.Request) {
				requests.Add(1)
				_, _ = io.WriteString(w, `{"answers":{}}`)
			})
			client := decide.NewDecisions(decide.DecisionsConfig{
				BaseURL: server.URL,
				Model:   "typesafe/jev-1.14",
				Token:   func(context.Context) (string, error) { return "token", nil },
			})
			questions := map[string]decide.Question{"question": test.question}
			if _, err := client.Ask(t.Context(), jsontext.Value(`{"ticket":"x"}`), questions); err == nil {
				t.Fatal("Ask succeeded")
			}
			if got := requests.Load(); got != 0 {
				t.Fatalf("request count = %d", got)
			}
		})
	}
}

func TestDecisionsAskHonorsConfiguredTimeout(t *testing.T) {
	t.Parallel()
	release := make(chan struct{})
	server := newDecisionsServer(t, "/", func(_ http.ResponseWriter, r *http.Request) {
		select {
		case <-r.Context().Done():
		case <-release:
		}
	})
	t.Cleanup(func() { close(release) })
	client := decide.NewDecisions(decide.DecisionsConfig{
		BaseURL: server.URL,
		Model:   "typesafe/jev-1.14",
		Token:   func(context.Context) (string, error) { return "token", nil },
		Timeout: 25 * time.Millisecond,
	})
	_, err := client.Ask(t.Context(), jsontext.Value(`{"ticket":"x"}`), map[string]decide.Question{"is_bug": {Type: decide.TypeNoul}})
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("Ask error = %v", err)
	}
}

func newDecisionsServer(t *testing.T, path string, handler http.HandlerFunc) *httptest.Server {
	t.Helper()
	mux := http.NewServeMux()
	mux.HandleFunc("POST "+path, handler)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server
}

func checkDecisionsRequest(t *testing.T, captured capturedRequest) {
	t.Helper()
	if got := captured.header.Get("Authorization"); got != "Bearer test-token" {
		t.Errorf("Authorization = %q", got)
	}
	if got := captured.header.Get("Content-Type"); got != "application/json" {
		t.Errorf("Content-Type = %q", got)
	}
	request := decodeObject(t, captured.bodyReader())
	if got := decodeString(t, request["model"]); got != "typesafe/jev-1.14" {
		t.Errorf("model = %q", got)
	}
	checkStateAndQuestions(t, requestValue(t, request))
}

func checkDecisionsAnswers(t *testing.T, answers map[string]decide.Answer) {
	t.Helper()
	if len(answers) != 3 {
		t.Fatalf("answer count = %d", len(answers))
	}
	team := answers["team"]
	if team.Choice != "backend" || team.Confidence != 0.75 || team.Probabilities["backend"] != 0.84 {
		t.Errorf("team = %+v", team)
	}
	checkNoul(t, answers["is_bug"], 0.96)
	checkScore(t, answers["urgency"], 1.99)
	if answers["urgency"].Probabilities["2"] != 0.99 {
		t.Errorf("urgency probabilities = %+v", answers["urgency"].Probabilities)
	}
}

func requestValue(t *testing.T, fields map[string]jsontext.Value) jsontext.Value {
	t.Helper()
	var out strings.Builder
	out.WriteByte('{')
	out.WriteString(`"state":`)
	out.Write(fields["state"])
	out.WriteString(`,"questions":`)
	out.Write(fields["questions"])
	out.WriteByte('}')
	return jsontext.Value(out.String())
}
