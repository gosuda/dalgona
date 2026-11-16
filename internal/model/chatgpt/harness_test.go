package chatgpt

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"net/url"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/model"
)

const liveBody = `{"models":[
 {"slug":"gpt-live-b","visibility":"list","priority":5,"context_window":200000,"max_context_window":900000,"effective_context_window_percent":95,"supported_reasoning_levels":[{"effort":"low"},{"effort":"high"},{"effort":"turbo"}],"input_modalities":["text"]},
 {"slug":"gpt-hidden","visibility":"hide","priority":1,"context_window":100000},
 {"slug":"gpt-live-a","visibility":"list","priority":2,"max_context_window":400000,"effective_context_window_percent":50,"max_output_tokens":32000,"supported_reasoning_levels":[{"effort":"medium"},{"effort":"ultra"},{"effort":"medium"}]},
 {"slug":"gpt-live-a","visibility":"list","priority":1,"context_window":1000},
 {"slug":"","visibility":"list","priority":0,"context_window":1000}
]}`

func liveIDs() []string {
	return []string{"gpt-live-a", "gpt-live-b"}
}

func staticIDs() []string {
	return []string{"gpt-6.1-sol", "gpt-6-astra", "gpt-6-sol", "gpt-6-luna", "gpt-5.6-sol", "gpt-6.1-terra", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"}
}

type seen struct {
	hits    int
	match   string
	auth    string
	account string
	uri     string
	query   string
}

type backend struct {
	mu       sync.Mutex
	status   int
	body     string
	etag     string
	location string
	drop     bool
	hang     bool
	last     seen
}

func okRegistry(version string) *backend {
	return &backend{status: http.StatusOK, body: `{"name":"@openai/codex","version":"` + version + `"}`, etag: `"npm-` + version + `"`}
}

func okModels() *backend {
	return &backend{status: http.StatusOK, body: liveBody}
}

func (b *backend) update(change func(*backend)) {
	b.mu.Lock()
	defer b.mu.Unlock()
	change(b)
}

func (b *backend) view() seen {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.last
}

func (b *backend) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	b.mu.Lock()
	b.last = seen{
		hits:    b.last.hits + 1,
		match:   r.Header.Get("If-None-Match"),
		auth:    r.Header.Get("Authorization"),
		account: r.Header.Get("chatgpt-account-id"),
		uri:     r.RequestURI,
		query:   r.URL.Query().Get("client_version"),
	}
	status, body, etag, location := b.status, b.body, b.etag, b.location
	drop, hang := b.drop, b.hang
	b.mu.Unlock()
	switch {
	case hang:
		<-r.Context().Done()
	case drop:
		closeConnection(w)
	default:
		reply(w, status, body, etag, location)
	}
}

func closeConnection(w http.ResponseWriter) {
	hijacker, ok := w.(http.Hijacker)
	if !ok {
		return
	}
	conn, _, err := hijacker.Hijack()
	if err == nil {
		_ = conn.Close()
	}
}

func reply(w http.ResponseWriter, status int, body, etag, location string) {
	if etag != "" {
		w.Header().Set("ETag", etag)
	}
	if location != "" {
		w.Header().Set("Location", location)
	}
	w.WriteHeader(status)
	_, _ = io.WriteString(w, body)
}

type reroute struct {
	local   *url.URL
	blocked *atomic.Int32
}

func (r reroute) RoundTrip(req *http.Request) (*http.Response, error) {
	outbound := req.Clone(req.Context())
	switch req.URL.Host {
	case "registry.npmjs.org":
		outbound.URL.Scheme, outbound.URL.Host, outbound.Host = r.local.Scheme, r.local.Host, r.local.Host
		outbound.URL.Path = "/registry" + req.URL.Path
		if req.URL.RawPath != "" {
			outbound.URL.RawPath = "/registry" + req.URL.RawPath
		}
	case r.local.Host:
	default:
		r.blocked.Add(1)
		return nil, fmt.Errorf("blocked request to %s", req.URL.Host)
	}
	return http.DefaultTransport.RoundTrip(outbound)
}

type site struct {
	url     string
	client  *http.Client
	blocked *atomic.Int32
}

func newSite(t *testing.T, registry, models *backend) site {
	t.Helper()
	mux := http.NewServeMux()
	mux.Handle("/registry/", registry)
	mux.Handle("/backend-api/codex/models", models)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	local, err := url.Parse(server.URL)
	if err != nil {
		t.Fatal(err)
	}
	blocked := new(atomic.Int32)
	t.Cleanup(func() {
		if count := blocked.Load(); count != 0 {
			t.Errorf("%d requests tried to leave the local server", count)
		}
	})
	return site{
		url:     server.URL + "/backend-api/codex",
		client:  &http.Client{Transport: reroute{local: local, blocked: blocked}},
		blocked: blocked,
	}
}

func tokenFor(account *string) func(context.Context) (oauth.Token, error) {
	return func(context.Context) (oauth.Token, error) {
		return oauth.Token{Access: "access-" + *account, Account: *account}, nil
	}
}

func (s site) config(dir string, account *string) Config {
	return Config{BaseURL: s.url, Client: s.client, CacheDir: dir, Token: tokenFor(account), Logger: slog.New(slog.DiscardHandler)}
}

type logbook struct{ buf bytes.Buffer }

func (l *logbook) logger() *slog.Logger {
	return slog.New(slog.NewTextHandler(&l.buf, &slog.HandlerOptions{Level: slog.LevelDebug}))
}

func (l *logbook) count(text string) int {
	return strings.Count(l.buf.String(), text)
}

type clock struct{ now time.Time }

func newClock() *clock {
	return &clock{now: time.Date(2026, time.November, 15, 8, 0, 0, 0, time.UTC)}
}

func (c *clock) Now() time.Time { return c.now }

func (c *clock) advance(by time.Duration) { c.now = c.now.Add(by) }

func ids(models []model.Model) []string {
	out := make([]string, len(models))
	for index, item := range models {
		out[index] = item.ID
	}
	return out
}

func listIDs(t *testing.T, p *Provider) []string {
	t.Helper()
	models, err := p.Models(t.Context())
	if err != nil {
		t.Fatalf("Models: %v", err)
	}
	return ids(models)
}

func expectIDs(t *testing.T, p *Provider, name string, want []string) {
	t.Helper()
	if got := listIDs(t, p); !slices.Equal(got, want) {
		t.Fatalf("%s: models = %v, want %v", name, got, want)
	}
}

func padded(size int, rows string) string {
	head := `{"models":[` + rows + `],"pad":"`
	tail := `"}`
	return head + strings.Repeat("a", size-len(head)-len(tail)) + tail
}
