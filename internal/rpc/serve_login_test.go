package rpc

import (
	"bytes"
	"context"
	"errors"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/model"
)

const loginKeyText = "sk-live-7f3a9c"

const rejectedText = "openai did not accept the API key; it was not saved"

type syncBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *syncBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *syncBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}

func readAuthFile(t *testing.T, home string) []byte {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(home, "auth.json"))
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func TestAPIKeyLoginVerifiesBeforeSaving(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	before := readAuthFile(t, r.home)
	c.fails("account/login/start", map[string]any{"type": "apiKey", "provider": "openai", "apiKey": "rejected-key"}, -32602, rejectedText)
	failed := c.await("account/login/completed", nil)
	expect(t, fieldOf(t, failed.Params, "success") == "false" && fieldOf(t, failed.Params, "provider") == "openai" && fieldOf(t, failed.Params, "error") == rejectedText, "failed login notice", string(failed.Params))
	if !bytes.Equal(readAuthFile(t, r.home), before) {
		t.Fatal("a rejected API key changed auth.json")
	}
	c.ok("account/login/start", map[string]any{"type": "apiKey", "provider": "openai", "apiKey": loginKeyText}, nil)
	done := c.await("account/login/completed", nil)
	expect(t, fieldOf(t, done.Params, "success") == "true" && fieldOf(t, done.Params, "provider") == "openai", "accepted login notice", string(done.Params))
	if !bytes.Contains(readAuthFile(t, r.home), []byte(loginKeyText)) {
		t.Fatal("an accepted API key was not stored")
	}
}

func TestAPIKeyLoginRefusesProviderWithoutAdapter(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	c.fails("account/login/start", map[string]any{"type": "apiKey", "provider": "anthropic", "apiKey": loginKeyText}, -32602, "anthropic did not accept the API key; it was not saved")
	if bytes.Contains(readAuthFile(t, r.home), []byte(loginKeyText)) {
		t.Fatal("a key for a provider with no adapter was stored")
	}
}

func TestRunnerServiceKeyIsStoredUnchecked(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.cfg.ProvidersWithKey = func(provider, _ string) (*model.Registry, error) {
		registry := model.NewRegistry()
		return registry, registry.Register(provider, refusingProvider{})
	}
	reopen(t, r)
	c := r.connect()
	c.ok("account/login/start", map[string]any{"type": "apiKey", "provider": "cloudflare", "apiKey": loginKeyText}, nil)
	done := c.await("account/login/completed", nil)
	expect(t, fieldOf(t, done.Params, "success") == "true" && fieldOf(t, done.Params, "provider") == "cloudflare", "runner service login notice", string(done.Params))
	if !bytes.Contains(readAuthFile(t, r.home), []byte(loginKeyText)) {
		t.Fatal("a runner service key was not stored")
	}
}

type refusingProvider struct{}

func (refusingProvider) Stream(context.Context, model.Request) (model.Stream, error) {
	return nil, errors.New("refused")
}

func (refusingProvider) Models(context.Context) ([]model.Model, error) {
	return nil, errors.New("refused")
}

func TestAPIKeyLoginRejectsMalformedInput(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	c.fails("account/login/start", map[string]any{"type": "apiKey", "provider": "openai", "apiKey": "   "}, -32602, "provider openai needs an apiKey")
	c.fails("account/login/start", map[string]any{"type": "apiKey", "provider": "openai", "apiKey": "sk a b"}, -32602, "the API key must be one line")
	c.fails("account/login/start", map[string]any{"type": "apiKey", "provider": "cursor", "apiKey": "sk-x"}, -32602, "provider cursor does not take an API key")
}

func TestAPIKeyNeverReachesLogsJournalOrModel(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	var logs syncBuffer
	logger := slog.New(slog.NewTextHandler(&logs, &slog.HandlerOptions{Level: slog.LevelDebug}))
	r.cfg.Logger = logger
	reopen(t, r)
	c := r.connectWith(ServeOptions{Logger: logger})
	c.ok("account/login/start", map[string]any{"type": "apiKey", "provider": "openai", "apiKey": loginKeyText}, nil)
	c.await("account/login/completed", nil)
	var started ThreadStartResponse
	c.ok("thread/start", map[string]any{"cwd": r.cwd, "model": "gpt-test"}, &started)
	c.await("thread/started", nil)
	startTurn(t, c, started.Thread.ID, "my key is "+loginKeyText)
	c.collect()
	if strings.Contains(logs.String(), loginKeyText) {
		t.Fatal("the API key reached the logs")
	}
	assertKeyAbsentFromModel(t, r)
	assertKeyAbsentFromHome(t, r.home)
}

func assertKeyAbsentFromModel(t *testing.T, r *rig) {
	t.Helper()
	r.llm.mu.Lock()
	bodies := slices.Clone(r.llm.bodies)
	r.llm.mu.Unlock()
	if len(bodies) == 0 {
		t.Fatal("the fake model received no request")
	}
	for _, body := range bodies {
		if strings.Contains(body, loginKeyText) {
			t.Fatal("the API key reached a model request")
		}
	}
}

func assertKeyAbsentFromHome(t *testing.T, home string) {
	t.Helper()
	root, err := os.OpenRoot(home)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = root.Close() }()
	err = fs.WalkDir(root.FS(), ".", func(path string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() || entry.Name() == "auth.json" || entry.Name() == "secrets.json" {
			return err
		}
		raw, err := fs.ReadFile(root.FS(), path)
		if err != nil {
			return err
		}
		if bytes.Contains(raw, []byte(loginKeyText)) {
			t.Fatalf("the API key reached %s", path)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

func TestOAuthLoginNamesProduct(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	r.cfg.Config.Product = config.ProductDalgona
	reopen(t, r)
	c := r.connect()
	c.fails("account/login/start", map[string]any{"type": "cursor"}, -32602, "login for cursor runs in the terminal: dalgona login cursor")
}

func TestAccountProvidersListLoginModes(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	c.ok("account/login/start", map[string]any{"type": "apiKey", "provider": "openai", "apiKey": loginKeyText}, nil)
	c.await("account/login/completed", nil)
	var list AccountProvidersResponse
	c.ok("dalgona/account/providers/list", map[string]any{}, &list)
	modes := map[string]string{}
	signedIn := map[string]bool{}
	for _, provider := range list.Providers {
		modes[provider.ID] = provider.Login
		signedIn[provider.ID] = provider.SignedIn
	}
	want := map[string]string{
		"anthropic": "apiKey", "google": "apiKey", "openai": "apiKey", "openrouter": "apiKey",
		"chatgpt": "browser", "antigravity": "terminal", "cursor": "terminal", "devin": "terminal",
		"cloudflare": "apiKey", "sandbox0": "apiKey", "sprites": "apiKey",
	}
	if !reflect.DeepEqual(modes, want) {
		t.Fatalf("login modes = %v, want %v", modes, want)
	}
	if !signedIn["openai"] || signedIn["anthropic"] {
		t.Fatalf("signed in = %v, want openai only", signedIn)
	}
}
