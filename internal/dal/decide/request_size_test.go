package decide

import (
	"context"
	"encoding/json/jsontext"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestRequestSizeCountsPostedBody(t *testing.T) {
	t.Parallel()
	var posted []byte
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		posted, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"answers":{"is_bug":{"type":"noul","noul":0.5}}}`)
	}))
	t.Cleanup(server.Close)
	client := NewDecisions(DecisionsConfig{
		BaseURL: server.URL,
		Model:   "m",
		Token:   func(context.Context) (string, error) { return "t", nil },
	})
	state := jsontext.Value(`{"task":"t"}`)
	questions := map[string]Question{"is_bug": {Type: TypeNoul, Criteria: map[string]string{"true": "yes", "false": "no"}}}
	if _, err := client.Ask(t.Context(), state, questions); err != nil {
		t.Fatal(err)
	}
	size, err := RequestSize(state, questions)
	if err != nil {
		t.Fatal(err)
	}
	const member = `"model":"m",`
	if got, want := len(posted)-size, len(member); got != want {
		t.Fatalf("posted body minus RequestSize = %d, want %d (the model member %s)", got, want, member)
	}
}
