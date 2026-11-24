package agent

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/openai"
)

const credentialTestProvider = "openai"

type credentialRig struct {
	store   *auth.Store
	server  *httptest.Server
	mu      sync.Mutex
	headers []string
	bodies  []string
	accept  string
	builds  atomic.Int64
}

func newCredentialRig(t *testing.T) *credentialRig {
	t.Helper()
	store, err := auth.NewWithPatterns(t.TempDir(), nil)
	if err != nil {
		t.Fatal(err)
	}
	rig := &credentialRig{store: store}
	mux := http.NewServeMux()
	mux.HandleFunc("/models", func(w http.ResponseWriter, r *http.Request) {
		header := r.Header.Get("Authorization")
		rig.mu.Lock()
		rig.headers = append(rig.headers, header)
		accept := rig.accept
		rig.mu.Unlock()
		if accept != "" && header != "Bearer "+accept {
			w.WriteHeader(http.StatusUnauthorized)
			_, _ = io.WriteString(w, `{"error":"bad key"}`)
			return
		}
		_, _ = io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`)
	})
	mux.HandleFunc("/responses", func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		rig.mu.Lock()
		rig.headers = append(rig.headers, r.Header.Get("Authorization"))
		rig.bodies = append(rig.bodies, string(body))
		rig.mu.Unlock()
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, sseText("done"))
	})
	rig.server = httptest.NewServer(mux)
	t.Cleanup(rig.server.Close)
	return rig
}

func (rig *credentialRig) setAccept(key string) {
	rig.mu.Lock()
	defer rig.mu.Unlock()
	rig.accept = key
}

func (rig *credentialRig) lastHeader() string {
	rig.mu.Lock()
	defer rig.mu.Unlock()
	if len(rig.headers) == 0 {
		return ""
	}
	return rig.headers[len(rig.headers)-1]
}

func (rig *credentialRig) bodiesSnapshot() []string {
	rig.mu.Lock()
	defer rig.mu.Unlock()
	return append([]string(nil), rig.bodies...)
}

func credentialAPIKey(rig *credentialRig) func(context.Context) (string, error) {
	return func(ctx context.Context) (string, error) {
		credential, err := rig.store.Token(ctx, credentialTestProvider)
		if err != nil {
			if errors.Is(err, auth.ErrNoCredential) {
				return "", fmt.Errorf("%w; run dalgona login openai", err)
			}
			return "", err
		}
		if credential.Key == "" {
			return "", errors.New("openai credential has no key; run dalgona login openai")
		}
		return credential.Key, nil
	}
}

func storeBackedHarness(t *testing.T, rig *credentialRig) *harness {
	t.Helper()
	t.Setenv("OPENAI_API_KEY", "")
	return newHarness(t, withApproval(config.ApprovalYolo), func(h *harness) {
		h.cfg.Auth = rig.store
		h.cfg.ProviderIDs = []string{credentialTestProvider}
		h.cfg.Providers = func(onRetry RetryFunc) (*model.Registry, error) {
			rig.builds.Add(1)
			if _, err := rig.store.Token(context.Background(), credentialTestProvider); err != nil {
				if errors.Is(err, auth.ErrNoCredential) {
					return nil, fmt.Errorf("%w; run dalgona login openai", err)
				}
				return nil, err
			}
			retry := model.DefaultRetryOptions()
			retry.Sleep = h.sleep
			retry.OnRetry = onRetry
			registry := model.NewRegistry()
			adapter := openai.New(openai.Config{BaseURL: rig.server.URL, APIKey: credentialAPIKey(rig), Retry: retry})
			if err := registry.Register(credentialTestProvider, adapter); err != nil {
				return nil, err
			}
			return registry, nil
		}
		h.cfg.ProvidersWithKey = func(provider, key string) (*model.Registry, error) {
			retry := model.DefaultRetryOptions()
			retry.Sleep = h.sleep
			adapter := openai.New(openai.Config{
				BaseURL: rig.server.URL,
				APIKey:  func(context.Context) (string, error) { return key, nil },
				Retry:   retry,
			})
			registry := model.NewRegistry()
			if err := registry.Register(provider, adapter); err != nil {
				return nil, err
			}
			return registry, nil
		}
	})
}

func mustSetAPIKey(t *testing.T, store *auth.Store, key string) {
	t.Helper()
	if err := store.SetAPIKey(credentialTestProvider, key); err != nil {
		t.Fatal(err)
	}
}

func mustListModels(t *testing.T, h *harness) []model.Model {
	t.Helper()
	models, err := h.host.Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	return models
}

func requireLoginError(t *testing.T, err error) {
	t.Helper()
	if err == nil || !errors.Is(err, auth.ErrNoCredential) {
		t.Fatalf("error = %v, want auth.ErrNoCredential", err)
	}
	if !strings.Contains(err.Error(), "run dalgona login openai") {
		t.Fatalf("error = %q, want the login hint", err.Error())
	}
}

func TestProviderCredentialRotationUsesLiveStore(t *testing.T) {
	rig := newCredentialRig(t)
	h := storeBackedHarness(t, rig)
	first := "sk-live-first-key-0001"
	second := "sk-live-second-key-0002"

	if _, err := h.host.Models(t.Context()); err == nil {
		t.Fatal("Models without a credential succeeded, want the login error")
	} else {
		requireLoginError(t, err)
	}

	mustSetAPIKey(t, rig.store, first)
	requireLiveModels(t, rig, h, first, "one listing")

	mustSetAPIKey(t, rig.store, second)
	requireLiveModels(t, rig, h, second, "rotation")

	if err := rig.store.Logout(credentialTestProvider); err != nil {
		t.Fatal(err)
	}
	if _, err := h.host.Models(t.Context()); err == nil {
		t.Fatal("Models after logout succeeded, want the login error")
	} else {
		requireLoginError(t, err)
	}
}

func requireLiveModels(t *testing.T, rig *credentialRig, h *harness, key, stage string) {
	t.Helper()
	before := rig.builds.Load()
	models := mustListModels(t, h)
	if len(models) != 1 || models[0].ID != "openai/gpt-test" {
		t.Fatalf("Models = %+v, want one openai/gpt-test model", models)
	}
	if got := rig.builds.Load(); got != before+1 {
		t.Fatalf("registry builds = %d, want %d after %s", got, before+1, stage)
	}
	if got, want := rig.lastHeader(), "Bearer "+key; got != want {
		t.Fatalf("Authorization = %q, want %q", got, want)
	}
}

func TestVerifyAPIKeyUsesCandidateWithoutStoring(t *testing.T) {
	rig := newCredentialRig(t)
	h := storeBackedHarness(t, rig)
	stored := "sk-live-second-key-0002"
	candidate := "sk-candidate-key-0003"
	mustSetAPIKey(t, rig.store, stored)

	rig.setAccept(candidate)
	if err := h.host.VerifyAPIKey(t.Context(), credentialTestProvider, candidate); err != nil {
		t.Fatalf("VerifyAPIKey with the accepted key = %v, want nil", err)
	}
	if got, want := rig.lastHeader(), "Bearer "+candidate; got != want {
		t.Fatalf("Authorization = %q, want %q", got, want)
	}
	kept, err := rig.store.Token(t.Context(), credentialTestProvider)
	if err != nil {
		t.Fatal(err)
	}
	if kept.Key != stored {
		t.Fatalf("stored key = %q, want %q unchanged", kept.Key, stored)
	}

	if verifyErr := h.host.VerifyAPIKey(t.Context(), credentialTestProvider, "sk-wrong-key-0004"); verifyErr == nil {
		t.Fatal("VerifyAPIKey with a rejected key succeeded, want an error")
	}
	kept, err = rig.store.Token(t.Context(), credentialTestProvider)
	if err != nil {
		t.Fatal(err)
	}
	if kept.Key != stored {
		t.Fatalf("stored key after rejection = %q, want %q unchanged", kept.Key, stored)
	}
}

func TestProviderCredentialNeverReachesJournalOrRequests(t *testing.T) {
	rig := newCredentialRig(t)
	h := storeBackedHarness(t, rig)
	liveKey := "sk-live-second-key-0002"
	mustSetAPIKey(t, rig.store, liveKey)

	thread := h.start()
	h.submit(thread, "summarize the key "+liveKey+" please")
	h.waitIdle(thread)

	for _, body := range rig.bodiesSnapshot() {
		if strings.Contains(body, liveKey) {
			t.Fatalf("model request body contains the raw credential: %.120q", body)
		}
	}
	found := false
	for _, body := range rig.bodiesSnapshot() {
		if strings.Contains(body, "{{secret:auth:openai}}") {
			found = true
		}
	}
	if !found {
		t.Fatal("model request bodies carry no credential placeholder, want masked prompt")
	}
	if logs := h.logs.String(); strings.Contains(logs, liveKey) {
		t.Fatalf("host logs contain the raw credential: %.120q", logs)
	}
	requireNoRawBytes(t, filepath.Join(h.home, "sessions", thread.ID()), liveKey)
	requireNoRawBytes(t, filepath.Join(h.home, "blobs"), liveKey)
}
