package app

import (
	"bytes"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/model/copilot"
)

const (
	copilotDonorClientID = "Iv1.b507a08c87ecfe98"
	copilotDeviceGrant   = "urn:ietf:params:oauth:grant-type:device_code"
	copilotLoginLine     = "Open https://github.com/login/device and enter the code WDJB-MJHT to log in to copilot.\n"
)

type pollAnswer func(w http.ResponseWriter)

func copilotGitHub(t *testing.T, answer pollAnswer) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if err := r.ParseForm(); err != nil {
			t.Errorf("parse form: %v", err)
		}
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.Path {
		case "/login/device/code":
			copilotDeviceRequest(t, w, r.Form)
		case "/login/oauth/access_token":
			copilotPoll(t, r.Form)
			answer(w)
		default:
			w.WriteHeader(http.StatusNotFound)
		}
	}))
	t.Cleanup(server.Close)
	return server
}

func copilotDeviceRequest(t *testing.T, w http.ResponseWriter, form url.Values) {
	t.Helper()
	if form.Get("client_id") != copilotDonorClientID || form.Get("scope") != "read:user" {
		t.Errorf("device code request = %v, want the donor client id and scope read:user", form)
	}
	_, _ = io.WriteString(w, `{"device_code":"dev-code","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":900,"interval":0}`)
}

func copilotPoll(t *testing.T, form url.Values) {
	t.Helper()
	if form.Get("client_id") != copilotDonorClientID || form.Get("device_code") != "dev-code" || form.Get("grant_type") != copilotDeviceGrant {
		t.Errorf("poll request = %v, want the donor client id, the device code and the device grant", form)
	}
}

func denied(w http.ResponseWriter) {
	_, _ = io.WriteString(w, `{"error":"access_denied"}`)
}

func pendingTwiceThenGranted(polls *atomic.Int32) pollAnswer {
	return func(w http.ResponseWriter) {
		if polls.Add(1) <= 2 {
			_, _ = io.WriteString(w, `{"error":"authorization_pending"}`)
			return
		}
		_, _ = io.WriteString(w, `{"access_token":"fake-github-login","token_type":"bearer","scope":"read:user"}`)
	}
}

func useCopilotServer(t *testing.T, server *httptest.Server) {
	t.Helper()
	base, ok := oauth.Providers["copilot"]
	if !ok {
		t.Fatal("oauth.Providers has no copilot entry")
	}
	provider := base
	provider.DeviceURL = server.URL + "/login/device/code"
	provider.TokenURL = server.URL + "/login/oauth/access_token"
	oauth.Providers["copilot"] = provider
	t.Cleanup(func() { oauth.Providers["copilot"] = base })
}

func copilotCredential(t *testing.T, home string) (auth.Credential, bool) {
	t.Helper()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range store.Entries() {
		if entry.ID == "copilot" {
			return entry.Credential, true
		}
	}
	return auth.Credential{}, false
}

func TestCopilotDeviceLoginStoresGitHubToken(t *testing.T) {
	var polls atomic.Int32
	useCopilotServer(t, copilotGitHub(t, pendingTwiceThenGranted(&polls)))
	home := t.TempDir()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	if err = Login(t.Context(), store, "copilot", strings.NewReader(""), &out); err != nil {
		t.Fatal(err)
	}
	if out.String() != copilotLoginLine {
		t.Fatalf("output = %q, want %q", out.String(), copilotLoginLine)
	}
	credential, found := copilotCredential(t, home)
	want := auth.Credential{Type: "oauth", Access: "fake-github-login"}
	if !found || credential != want || polls.Load() != 3 {
		t.Fatalf("credential = %+v (found %v) after %d polls, want %+v with no refresh token and no expiry after 3 polls", credential, found, polls.Load(), want)
	}
}

func TestCopilotDeviceLoginDenialStoresNothing(t *testing.T) {
	useCopilotServer(t, copilotGitHub(t, denied))
	home := t.TempDir()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	err = Login(t.Context(), store, "copilot", strings.NewReader(""), &out)
	if err == nil || err.Error() != "login copilot: device authorization failed: access_denied" {
		t.Fatalf("Login error = %v, want the denial", err)
	}
	if _, found := copilotCredential(t, home); found || out.String() != copilotLoginLine {
		t.Fatalf("stored = %v, output = %q; want nothing stored after the prompt", found, out.String())
	}
}

func TestLoginDeviceAuthRejectsCopilot(t *testing.T) {
	var stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDalgona, Home: t.TempDir(), Stdout: &bytes.Buffer{}, Stderr: &stderr}
	if code := RunCLI(t.Context(), env, []string{"login", "copilot", "--device-auth"}); code != 1 {
		t.Fatalf("copilot --device-auth exit %d", code)
	}
	if !strings.Contains(stderr.String(), "login copilot: --device-auth works only for chatgpt") {
		t.Fatalf("stderr %q", stderr.String())
	}
}

func TestRegistryOffersCopilotOnlyWithACredential(t *testing.T) {
	store, err := auth.Load(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	creds := newCredentials(store, config.ProductDalgona, nil)
	registry, err := creds.registry(config.Config{}, nil)
	if err != nil {
		t.Fatal(err)
	}
	provider, ok := registry.Provider("copilot")
	if _, listed := provider.(*copilot.Provider); !ok || listed {
		t.Fatalf("provider without a credential = %T (registered %v), want the unlisted wrapper", provider, ok)
	}
	checkCopilotCredentialRegistered(t, store, creds)
}

func checkCopilotCredentialRegistered(t *testing.T, store *auth.Store, creds *credentials) {
	t.Helper()
	if err := store.SetCredential("copilot", auth.Credential{Type: "oauth", Access: "fake-github-login"}); err != nil {
		t.Fatal(err)
	}
	registry, err := creds.registry(config.Config{}, nil)
	if err != nil {
		t.Fatal(err)
	}
	provider, ok := registry.Provider("copilot")
	if _, listed := provider.(*copilot.Provider); !ok || !listed {
		t.Fatalf("provider with a credential = %T (registered %v), want *copilot.Provider", provider, ok)
	}
	if got, tokenErr := creds.access("copilot")(t.Context()); tokenErr != nil || got != "fake-github-login" {
		t.Fatalf("copilot access = %q, %v; want the stored GitHub credential", got, tokenErr)
	}
}
