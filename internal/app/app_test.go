package app

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"errors"
	"fmt"
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
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/scripthost"
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
	token, err := newCredentials(store, config.ProductDalgona, nil).chatgptToken(t.Context())
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
	if token, err := creds.chatgptToken(t.Context()); err != nil || token.Access != "first-access" {
		t.Fatalf("first token = %+v, %v", token, err)
	}
	second := auth.Credential{Type: "oauth", Access: "second-access", Refresh: "second-refresh", Account: "acct-2"}
	if err := loadStore(t, home).SetCredential("chatgpt", second); err != nil {
		t.Fatal(err)
	}
	if token, err := creds.chatgptToken(t.Context()); err != nil || token.Access != "second-access" || token.Account != "acct-2" {
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
	err = Login(t.Context(), store, "openai", strings.NewReader("\n"), io.Discard)
	if err == nil || err.Error() != "login openai: no API key was entered" {
		t.Fatalf("empty key error = %v", err)
	}
	err = Login(t.Context(), store, "anthropic", strings.NewReader("\n"), io.Discard)
	if err == nil || err.Error() != "login anthropic: oauth paste returned no authorization code" {
		t.Fatalf("empty anthropic login error = %v, want the OAuth paste failure", err)
	}
	if _, statErr := os.Stat(filepath.Join(store.Dir(), "auth.json")); !os.IsNotExist(statErr) {
		t.Fatalf("failed logins wrote auth.json: %v", statErr)
	}
}

func TestProviderIDsIncludeConfiguredEndpoints(t *testing.T) {
	ids := providerIDs(config.Config{Providers: map[string]config.ProviderSpec{"OpenRouter": {BaseURL: "https://openrouter.ai/api/v1"}, "openai": {}}})
	want := []string{"anthropic", "antigravity", "chatgpt", "copilot", "cursor", "devin", "google", "openai", "openrouter"}
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
		var names []string
		for _, spec := range ext.Visible(toolSpecs(tools)) {
			names = append(names, spec.Name)
		}
		if want := []string{"read", "search", "patch", "exec", "eval"}; !slices.Equal(names, want) {
			t.Fatalf("%s visible tools = %v, want %v", product, names, want)
		}
		runProductCell(t, string(product), cfg.ToolMode, tools)
	}
}

type productCellCase struct {
	name   string
	mode   config.ToolMode
	uses   []string
	set    bool
	status string
}

func toolSpecs(tools []tool.Tool) []tool.Spec {
	specs := make([]tool.Spec, 0, len(tools))
	for _, item := range tools {
		specs = append(specs, item.Spec())
	}
	return specs
}

func runProductCell(t *testing.T, product string, mode config.ToolMode, tools []tool.Tool) {
	t.Helper()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "fixture.txt"), []byte("product cell bytes"), 0o644); err != nil {
		t.Fatal(err)
	}
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	env := tool.Env{Runner: local, Cwd: dir, Snapshots: snapshot.New()}
	for _, c := range []productCellCase{
		{product + " eval-first", mode, nil, false, "completed"},
		{product + " normal", config.ToolModeNormal, nil, false, "completed"},
		{product + " empty uses", mode, []string{}, true, "denied"},
	} {
		runProductCellCase(t, c, tools, env)
	}
	specs := toolSpecs(tools)
	withoutEval := make([]tool.Spec, 0, len(specs))
	for _, spec := range specs {
		if spec.Name != script.EvalName {
			withoutEval = append(withoutEval, spec)
		}
	}
	if slices.Contains(specNames(script.ModelTools(mode, withoutEval)), script.EvalName) {
		t.Fatalf("%s offers eval after it was dropped from the tool list", product)
	}
	if err := script.CheckDirect(mode, "web_fetch"); err == nil {
		t.Fatalf("%s allows a direct folded call when no cell tool exists", product)
	}
}

func runProductCellCase(t *testing.T, c productCellCase, tools []tool.Tool, env tool.Env) {
	t.Helper()
	env.Home = t.TempDir()
	j := &cellJournal{tools: tools, env: env}
	eval := script.NewEval(j, script.EvalOptions{Mode: c.mode, Tools: toolSpecs(tools), Config: config.EvalConfig{Uses: c.uses, Set: c.set}})
	got := runCell(t, eval, `{"code":"tools.read(path = 'fixture.txt')"}`)
	if got.Status != c.status {
		t.Fatalf("%s cell status = %s, want %s, error = %+v", c.name, got.Status, c.status, got.Error)
	}
	if c.status == "completed" {
		requireCompletedRead(t, c.name, got, j)
		return
	}
	if j.calls != 0 {
		t.Fatalf("%s denied cell still performed %d reads", c.name, j.calls)
	}
}

func requireCompletedRead(t *testing.T, name string, got cellReply, j *cellJournal) {
	t.Helper()
	if !strings.Contains(decodeString(t, got.Value), "1:product cell bytes\n") || j.calls != 1 {
		t.Fatalf("%s cell value = %s, calls = %d", name, got.Value, j.calls)
	}
}

type cellReply struct {
	Status string         `json:"status"`
	Value  jsontext.Value `json:"value"`
	Error  *struct {
		Code    string `json:"code"`
		Message string `json:"message"`
	} `json:"error"`
}

func specNames(list []tool.Spec) []string {
	out := make([]string, len(list))
	for i, spec := range list {
		out[i] = spec.Name
	}
	return out
}

func runCell(t *testing.T, eval *script.Eval, args string) cellReply {
	t.Helper()
	result, err := eval.Run(t.Context(), tool.Call{ID: "cell-1", Name: script.EvalName, Args: jsontext.Value(args)}, tool.Env{})
	if err != nil {
		t.Fatalf("cell run: %v", err)
	}
	var reply cellReply
	if err := json.Unmarshal(result.Output, &reply); err != nil {
		t.Fatalf("decode cell reply %s: %v", result.Output, err)
	}
	return reply
}

type cellJournal struct {
	tools []tool.Tool
	env   tool.Env
	calls int
}

func (c *cellJournal) Started(start script.RunStart) (script.RunStart, *script.Outcome, error) {
	return start, nil, nil
}

func (c *cellJournal) Lookup(string, int) (script.Recorded, bool) { return script.Recorded{}, false }

func (c *cellJournal) Record(script.Slot, jsontext.Value) error { return nil }

func (c *cellJournal) Completed(string, script.Outcome) error { return nil }

func (c *cellJournal) CallTool(ctx context.Context, _ script.Slot, name string, args jsontext.Value) (jsontext.Value, error) {
	for _, item := range c.tools {
		if item.Spec().Name != name {
			continue
		}
		c.calls++
		result, err := item.Run(ctx, tool.Call{ID: "cell-tool", Name: name, Args: args}, c.env)
		if err != nil {
			return nil, err
		}
		return jsontext.AppendQuote(nil, string(result.Output))
	}
	return nil, fmt.Errorf("cell journal: tool %s is not registered", name)
}

func (c *cellJournal) Spawn(context.Context, script.Slot, string, script.SpawnOpts) (string, error) {
	return "", errors.New("cell journal: spawn is unused")
}

func (c *cellJournal) Wait(context.Context, []string) ([]string, error) {
	return nil, errors.New("cell journal: wait is unused")
}

func (c *cellJournal) Judge(context.Context, script.Slot, script.JudgeRequest) (jsontext.Value, error) {
	return nil, errors.New("cell journal: judge is unused")
}

func (c *cellJournal) Log(string, string) {}

func decodeString(t *testing.T, v jsontext.Value) string {
	t.Helper()
	var s string
	if err := json.Unmarshal([]byte(v), &s); err != nil {
		t.Fatalf("reply value is not a JSON string: %v", err)
	}
	return s
}
