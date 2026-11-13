package app

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/scripthost"
	"github.com/gosuda/dalgona/internal/tool"
)

type keyRecorder struct {
	mu   sync.Mutex
	keys []string
}

func (k *keyRecorder) serve(w http.ResponseWriter, r *http.Request) {
	k.mu.Lock()
	k.keys = append(k.keys, r.Header.Get("Authorization"))
	k.mu.Unlock()
	w.Header().Set("Content-Type", "text/event-stream")
	_, _ = io.WriteString(w, "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n")
}

func (k *keyRecorder) seen() []string {
	k.mu.Lock()
	defer k.mu.Unlock()
	return slices.Clone(k.keys)
}

func streamOnce(t *testing.T, registry *model.Registry) {
	t.Helper()
	stream, err := registry.Stream(t.Context(), "openai", model.Request{Model: "gpt-test", Messages: []model.Message{{Role: "user", Text: "hi"}}})
	if err != nil {
		t.Fatal(err)
	}
	for {
		if _, err := stream.Next(t.Context()); err != nil {
			break
		}
	}
	if err := stream.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestRegistryReadsKeysStoredByAnotherProcess(t *testing.T) {
	home := t.TempDir()
	recorder := &keyRecorder{}
	server := httptest.NewServer(http.HandlerFunc(recorder.serve))
	t.Cleanup(server.Close)
	t.Setenv("OPENAI_BASE_URL", server.URL)
	t.Setenv("OPENAI_API_KEY", "")
	daemonStore, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	creds := newCredentials(daemonStore, config.ProductDalgona, nil)
	cfg := config.Config{StreamStartTimeout: time.Minute, StreamIdleTimeout: time.Minute}
	registry, err := creds.registry(cfg, nil)
	if err != nil {
		t.Fatal(err)
	}
	login, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	if err := Login(t.Context(), login, "openai", strings.NewReader("first-key\n"), io.Discard); err != nil {
		t.Fatal(err)
	}
	streamOnce(t, registry)
	if err := login.SetAPIKey("openai", "second-key"); err != nil {
		t.Fatal(err)
	}
	streamOnce(t, registry)
	want := []string{"Bearer first-key", "Bearer second-key"}
	if got := recorder.seen(); !slices.Equal(got, want) {
		t.Fatalf("Authorization headers = %q, want %q", got, want)
	}
}

func refreshServer(t *testing.T, refreshes *atomic.Int32) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		if !bytes.Contains(body, []byte("stale-refresh")) {
			http.Error(w, "wrong refresh token", http.StatusBadRequest)
			return
		}
		refreshes.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"access_token":"fresh-access","refresh_token":"fresh-refresh","expires_in":3600}`)
	}))
	t.Cleanup(server.Close)
	provider := oauth.Providers["chatgpt"]
	oauth.Providers["chatgpt"] = withTokenURL(provider, server.URL)
	t.Cleanup(func() { oauth.Providers["chatgpt"] = provider })
	return server
}

func TestOAuthRefreshIsSavedThroughTheHostStore(t *testing.T) {
	home := t.TempDir()
	t.Setenv("CHATGPT_API_KEY", "")
	var refreshes atomic.Int32
	refreshServer(t, &refreshes)
	store := loadStore(t, home)
	expired := auth.Credential{Type: "oauth", Access: "stale-access", Refresh: "stale-refresh", Expires: "2020-01-01T00:00:00Z", Account: "acct-7"}
	if setErr := store.SetCredential("chatgpt", expired); setErr != nil {
		t.Fatal(setErr)
	}
	token, err := newCredentials(store, config.ProductDalgona, nil).token(t.Context(), "chatgpt")
	if err != nil {
		t.Fatal(err)
	}
	if token.Expires.Before(time.Now()) || refreshes.Load() != 1 {
		t.Fatalf("token expires %v after %d refreshes", token.Expires, refreshes.Load())
	}
	token.Expires = time.Time{}
	if want := (oauth.Token{Access: "fresh-access", Refresh: "fresh-refresh", Account: "acct-7"}); token != want {
		t.Fatalf("token = %+v, want %+v", token, want)
	}
	saved, err := loadCredential(t, home, "chatgpt")
	if err != nil || saved.Expires == "" {
		t.Fatalf("saved credential = %+v, %v", saved, err)
	}
	saved.Expires = ""
	if want := (auth.Credential{Type: "oauth", Access: "fresh-access", Refresh: "fresh-refresh", Account: "acct-7"}); saved != want {
		t.Fatalf("saved credential = %+v, want %+v", saved, want)
	}
}

func loadStore(t *testing.T, home string) *auth.Store {
	t.Helper()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	return store
}

func loadCredential(t *testing.T, home, provider string) (auth.Credential, error) {
	t.Helper()
	store, err := auth.Load(home)
	if err != nil {
		return auth.Credential{}, err
	}
	return store.Token(t.Context(), provider)
}

func TestOAuthReloginInAnotherProcessReplacesCachedToken(t *testing.T) {
	home := t.TempDir()
	t.Setenv("CHATGPT_API_KEY", "")
	store := loadStore(t, home)
	first := auth.Credential{Type: "oauth", Access: "first-access", Refresh: "first-refresh", Account: "acct-1"}
	if err := store.SetCredential("chatgpt", first); err != nil {
		t.Fatal(err)
	}
	creds := newCredentials(store, config.ProductDalgona, nil)
	if token, err := creds.token(t.Context(), "chatgpt"); err != nil || token.Access != "first-access" {
		t.Fatalf("first token = %+v, %v", token, err)
	}
	second := auth.Credential{Type: "oauth", Access: "second-access", Refresh: "second-refresh", Account: "acct-2"}
	if err := loadStore(t, home).SetCredential("chatgpt", second); err != nil {
		t.Fatal(err)
	}
	if token, err := creds.token(t.Context(), "chatgpt"); err != nil || token.Access != "second-access" || token.Account != "acct-2" {
		t.Fatalf("token after relogin = %+v, %v", token, err)
	}
}

func clearProviderKeys(t *testing.T) {
	t.Helper()
	for _, key := range []string{
		"OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "DEVIN_API_KEY", "OPENROUTER_API_KEY",
		"CHATGPT_API_KEY", "ANTIGRAVITY_API_KEY", "CURSOR_API_KEY", "GOOGLE_API_KEY",
	} {
		t.Setenv(key, "")
	}
}

func modelIDs(t *testing.T, host *agent.Host) []string {
	t.Helper()
	models, err := host.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	ids := make([]string, 0, len(models))
	for _, item := range models {
		ids = append(ids, item.ID)
	}
	return ids
}

func TestModelListOffersOnlyProvidersWithACredential(t *testing.T) {
	home := t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	clearProviderKeys(t)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/models" || r.Header.Get("Authorization") != "Bearer listed-key" {
			http.Error(w, "unexpected request", http.StatusUnauthorized)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`)
	}))
	t.Cleanup(server.Close)
	t.Setenv("OPENAI_BASE_URL", server.URL)
	var logs bytes.Buffer
	logger := slog.New(slog.NewTextHandler(&logs, &slog.HandlerOptions{Level: slog.LevelDebug}))
	host, closeHost, err := NewHost(t.Context(), Options{Product: config.ProductDalgona, Cwd: t.TempDir(), Logger: logger})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = closeHost() })
	if ids := modelIDs(t, host); len(ids) != 0 {
		t.Fatalf("models with an empty home = %q, want none", ids)
	}
	if text := logs.String(); strings.Contains(text, "level=WARN") || !strings.Contains(text, "level=DEBUG msg=\"app: model listing skipped a provider without a credential\" provider=chatgpt") {
		t.Fatalf("skipped providers must log at debug only:\n%s", text)
	}
	if err := loadStore(t, home).SetAPIKey("openai", "listed-key"); err != nil {
		t.Fatal(err)
	}
	if ids, want := modelIDs(t, host), []string{"openai/gpt-test"}; !slices.Equal(ids, want) {
		t.Fatalf("models after openai login = %q, want %q", ids, want)
	}
}

func TestMissingCredentialNamesTheLoginCommand(t *testing.T) {
	clearProviderKeys(t)
	store := loadStore(t, t.TempDir())
	for _, test := range []struct {
		product config.Product
		want    string
	}{
		{config.ProductDal, "auth: no credential for openai; run dal login openai"},
		{config.ProductDalgona, "auth: no credential for openai; run dalgona login openai"},
		{"", "auth: no credential for openai; run dalgona login openai"},
	} {
		_, err := newCredentials(store, test.product, nil).apiKey("openai", "")(t.Context())
		if err == nil || err.Error() != test.want || !errors.Is(err, auth.ErrNoCredential) {
			t.Fatalf("product %q error = %v, want %q", test.product, err, test.want)
		}
	}
}

func withTokenURL(provider oauth.Provider, url string) oauth.Provider {
	provider.TokenURL = url
	return provider
}

func TestLoginRejectsUnknownAndEmpty(t *testing.T) {
	store, err := auth.Load(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	err = Login(t.Context(), store, "nope", strings.NewReader(""), io.Discard)
	if err == nil || !strings.HasPrefix(err.Error(), `unknown provider "nope"; known providers: anthropic, antigravity, chatgpt`) {
		t.Fatalf("unknown provider error = %v", err)
	}
	err = Login(t.Context(), store, "anthropic", strings.NewReader("\n"), io.Discard)
	if err == nil || err.Error() != "login anthropic: no API key was entered" {
		t.Fatalf("empty key error = %v", err)
	}
	if _, statErr := os.Stat(filepath.Join(store.Dir(), "auth.json")); !os.IsNotExist(statErr) {
		t.Fatalf("failed logins wrote auth.json: %v", statErr)
	}
}

func TestProviderIDsIncludeConfiguredEndpoints(t *testing.T) {
	ids := providerIDs(config.Config{Providers: map[string]config.ProviderSpec{"OpenRouter": {BaseURL: "https://openrouter.ai/api/v1"}, "openai": {}}})
	want := []string{"anthropic", "antigravity", "chatgpt", "cursor", "devin", "google", "openai", "openrouter"}
	if !slices.Equal(ids, want) {
		t.Fatalf("provider ids = %q, want %q", ids, want)
	}
}

func TestContextWindowsSplitProviderAndModel(t *testing.T) {
	windows := contextWindows(map[string]config.ModelSpec{
		"openai/gpt-test": {ContextWindow: 1000}, "bad": {ContextWindow: 5}, "google/g": {},
	})
	if len(windows) != 1 || windows["openai"]["gpt-test"] != 1000 {
		t.Fatalf("context windows = %v", windows)
	}
}

type namedTool struct{ tool.Tool }

func (namedTool) Spec() tool.Spec { return tool.Spec{Name: "fix__echo"} }

func TestToolSetAppendsBatteryToolsForEveryModel(t *testing.T) {
	var asked string
	set := toolSet(config.Config{EditStyle: config.EditStyleSpec{Default: "anchor"}}, func(ctx context.Context, cwd string) []tool.Tool {
		asked = cwd
		return []tool.Tool{namedTool{}}
	})
	tools, err := set(t.Context(), "gpt-test", "/project")
	if asked != "/project" {
		t.Fatalf("battery tools built for cwd %q, want /project", asked)
	}
	if err != nil {
		t.Fatal(err)
	}
	names := make([]string, 0, len(tools))
	for _, item := range tools {
		names = append(names, item.Spec().Name)
	}
	for _, want := range []string{"read", "exec", "todo", "web_fetch", "fix__echo"} {
		if !slices.Contains(names, want) {
			t.Fatalf("tool set %q lacks %s", names, want)
		}
	}
}

func TestToolSetListsRewriteAfterPatch(t *testing.T) {
	set := toolSet(config.Config{EditStyle: config.EditStyleSpec{Default: "anchor"}}, func(context.Context, string) []tool.Tool { return nil })
	tools, err := set(t.Context(), "gpt-test", "/project")
	if err != nil {
		t.Fatal(err)
	}
	names := make([]string, 0, len(tools))
	for _, item := range tools {
		names = append(names, item.Spec().Name)
	}
	at := slices.Index(names, "patch")
	if at < 0 || at+1 >= len(names) || names[at+1] != "rewrite" {
		t.Fatalf("tool set %q lists rewrite after patch in no position", names)
	}
}

func TestDeciderUsesTheActionThreadModel(t *testing.T) {
	t.Setenv("DALGONA_HOME", t.TempDir())
	host, closeHost, err := NewHost(t.Context(), Options{Product: config.ProductDalgona, Cwd: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = closeHost() })
	thread, err := host.Start(t.Context(), agent.ThreadSpec{Cwd: t.TempDir(), Model: "anthropic/claude-test"})
	if err != nil {
		t.Fatal(err)
	}
	roles := &roleResolver{}
	if got := roles.threadModel(thread.ID(), "openai/fallback"); got != "openai/fallback" {
		t.Fatalf("model before the host is attached = %q", got)
	}
	roles.set(host)
	if got := roles.threadModel(thread.ID(), "openai/fallback"); got != "anthropic/claude-test" {
		t.Fatalf("thread model = %q, want anthropic/claude-test", got)
	}
	if got := roles.threadModel("missing", "openai/fallback"); got != "openai/fallback" {
		t.Fatalf("unknown thread model = %q", got)
	}
}

func TestDefaultToolModeShowsOnlyTheEvalFirstList(t *testing.T) {
	for _, product := range []config.Product{config.ProductDal, config.ProductDalgona} {
		home := "DALGONA_HOME"
		if product == config.ProductDal {
			home = "DAL_HOME"
		}
		t.Setenv(home, t.TempDir())
		cfg, err := config.Load(product, t.TempDir(), config.Flags{})
		if err != nil {
			t.Fatal(err)
		}
		builtin, err := toolSet(cfg, func(context.Context, string) []tool.Tool { return nil })(t.Context(), "openai/gpt-test", t.TempDir())
		if err != nil {
			t.Fatal(err)
		}
		ext := scripthost.New(scripthost.Config{Mode: cfg.ToolMode})(nil)
		tools := ext.Tools(builtin)
		specs := make([]tool.Spec, 0, len(tools))
		for _, item := range tools {
			specs = append(specs, item.Spec())
		}
		var names []string
		for _, spec := range ext.Visible(specs) {
			names = append(names, spec.Name)
		}
		if want := []string{"read", "search", "patch", "exec", "eval"}; !slices.Equal(names, want) {
			t.Fatalf("%s visible tools = %v, want %v", product, names, want)
		}
	}
}
