package doctor

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"
)

type listServer struct {
	*httptest.Server
	mu       sync.Mutex
	requests []string
	headers  []string
}

func (s *listServer) seen() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Clone(s.requests)
}

func (s *listServer) authorizations() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Clone(s.headers)
}

func newListServer(t *testing.T, handle http.HandlerFunc) *listServer {
	t.Helper()
	server := &listServer{}
	server.Server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		server.mu.Lock()
		server.requests = append(server.requests, r.Method+" "+r.URL.Path)
		server.headers = append(server.headers, r.Header.Get("Authorization"))
		server.mu.Unlock()
		handle(w, r)
	}))
	t.Cleanup(server.Close)
	return server
}

func modelsOf(count int) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		items := make([]string, count)
		for index := range items {
			items[index] = fmt.Sprintf(`{"id":"m%d"}`, index)
		}
		fmt.Fprintf(w, `{"data":[%s]}`, strings.Join(items, ","))
	}
}

func providerConfig(t *testing.T, home string, servers map[string]*listServer) {
	t.Helper()
	var text strings.Builder
	for id, server := range servers {
		fmt.Fprintf(&text, "[providers.%s]\nbase_url = %q\napi_key_env = \"%s_KEY\"\n\n", id, server.URL, strings.ToUpper(id))
		t.Setenv(strings.ToUpper(id)+"_KEY", "")
	}
	writeFile(t, filepath.Join(home, "config.toml"), text.String())
	store := storeOf(t, home)
	for id := range servers {
		if err := store.SetAPIKey(id, stubKey+"-"+id); err != nil {
			t.Fatal(err)
		}
	}
}

func TestOfflineMakesNoProviderCalls(t *testing.T) {
	home := newHome(t)
	server := newListServer(t, modelsOf(3))
	providerConfig(t, home, map[string]*listServer{"stub": server})
	h := newHarness(t, home)
	got, code := h.run(t)
	if requests := server.seen(); len(requests) != 0 {
		t.Fatalf("offline run sent %q", requests)
	}
	expectLines(t, only(got, "providers", "provider-models"), []string{"ok providers: stub: api key set"})
	expectCode(t, code, 0)

	h = newHarness(t, home)
	h.in.env.Online = true
	got, code = h.run(t)
	if requests := server.seen(); len(requests) != 1 {
		t.Fatalf("online run sent %q, want one request", requests)
	}
	expectLines(t, only(got, "provider-models"), []string{"ok provider-models: stub: 3 models"})
	expectCode(t, code, 0)
}

func TestOnlineListsOncePerProvider(t *testing.T) {
	home := newHome(t)
	first := newListServer(t, modelsOf(2))
	second := newListServer(t, modelsOf(5))
	providerConfig(t, home, map[string]*listServer{"alpha": first, "beta": second})
	h := newHarness(t, home)
	h.in.env.Online = true
	got, code := h.run(t)
	expectLines(t, only(got, "provider-models"), []string{
		"ok provider-models: alpha: 2 models",
		"ok provider-models: beta: 5 models",
	})
	expectCode(t, code, 0)
	for name, server := range map[string]*listServer{"alpha": first, "beta": second} {
		if requests := server.seen(); len(requests) != 1 || requests[0] != "GET /models" {
			t.Fatalf("%s got %q, want one GET /models", name, requests)
		}
	}
	for index, header := range first.authorizations() {
		if header != "Bearer "+stubKey+"-alpha" {
			t.Fatalf("alpha request %d sent authorization %q", index, header)
		}
	}
}

func TestOnlineFailureIsOneMaskedFailRowWithoutRetry(t *testing.T) {
	home := newHome(t)
	server := newListServer(t, func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusServiceUnavailable)
		fmt.Fprint(w, "rejected "+stubKey+"-stub")
	})
	providerConfig(t, home, map[string]*listServer{"stub": server})
	h := newHarness(t, home)
	h.in.env.Online = true
	got, code := h.run(t)
	expectLines(t, only(got, "provider-models"), []string{
		"fail provider-models: stub: chatcompat: models http 503: rejected {{secret:auth:stub}}",
	})
	expectCode(t, code, 1)
	if requests := server.seen(); len(requests) != 1 {
		t.Fatalf("a failed list was sent %d times, want 1", len(requests))
	}
	if strings.Contains(h.out.String(), stubKey) {
		t.Fatalf("output holds the credential: %q", h.out.String())
	}
}

func TestOnlineTimeoutFailsTheRow(t *testing.T) {
	home := newHome(t)
	server := newListServer(t, func(_ http.ResponseWriter, r *http.Request) { <-r.Context().Done() })
	providerConfig(t, home, map[string]*listServer{"stub": server})
	h := newHarness(t, home)
	h.in.env.Online = true
	h.in.timeout = 50 * time.Millisecond
	got, code := h.run(t)
	rows := only(got, "provider-models")
	if len(rows) != 1 || !strings.HasPrefix(rows[0], "fail provider-models: stub: chatcompat: models request: ") || !strings.Contains(rows[0], "deadline exceeded") {
		t.Fatalf("rows = %q", rows)
	}
	expectCode(t, code, 1)
}

func TestOnlineSkipsAProviderWithoutAModelList(t *testing.T) {
	home := newHome(t)
	if err := storeOf(t, home).SetAPIKey("openrouter", stubKey); err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, home)
	h.in.env.Online = true
	got, code := h.run(t)
	expectLines(t, only(got, "provider-models"), []string{"warn provider-models: openrouter: not configured; no request made"})
	expectCode(t, code, 0)
}
