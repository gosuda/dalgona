package app

import (
	"bytes"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/config"
)

func TestLoginRunnerServiceAPIKeyStoresTheRemoteToken(t *testing.T) {
	for _, service := range []string{"sprites", "sandbox0", "cloudflare"} {
		t.Run(service, func(t *testing.T) {
			home := t.TempDir()
			var stdout, stderr bytes.Buffer
			env := CLIEnv{Product: config.ProductDalgona, Home: home, Stdout: &stdout, Stderr: &stderr}
			if code := RunCLI(t.Context(), env, []string{"login", service, "--api-key", "tok-1"}); code != 0 {
				t.Fatalf("login %s exit %d: %s", service, code, stderr.String())
			}
			if got := apiKeyIn(t, home, service); got != "tok-1" {
				t.Fatalf("%s key = %q, want tok-1", service, got)
			}
		})
	}
}

func TestRemoteLoginRunnerServiceAPIKeyStoresItInTheBox(t *testing.T) {
	box := newServedBox(t)
	clientHome := t.TempDir()
	var stdout, stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDalgona, Home: clientHome, Remote: "box", Dial: box.dial, Stdout: &stdout, Stderr: &stderr}
	if code := RunCLI(t.Context(), env, []string{"login", "sprites", "--api-key", "tok-1"}); code != 0 {
		t.Fatalf("remote sprites login exit %d: %s", code, stderr.String())
	}
	if got := apiKeyIn(t, box.home, "sprites"); got != "tok-1" {
		t.Fatalf("box sprites key = %q, want tok-1", got)
	}
	if got := apiKeyIn(t, clientHome, "sprites"); got != "" {
		t.Fatalf("client sprites key = %q, want empty", got)
	}
}

func TestLoginAPIKeyStillRequiresAKeyProvider(t *testing.T) {
	var stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDalgona, Home: t.TempDir(), Stdout: &bytes.Buffer{}, Stderr: &stderr}
	if code := RunCLI(t.Context(), env, []string{"login", "antigravity", "--api-key", "k"}); code != 1 {
		t.Fatalf("antigravity --api-key exit %d", code)
	}
	if !strings.Contains(stderr.String(), "--api-key works only for") {
		t.Fatalf("stderr %q", stderr.String())
	}
}

func TestLoginDeviceAuthSignsInWithoutABrowser(t *testing.T) {
	var polls atomic.Int32
	server := httptest.NewServer(chatGPTDeviceServer(t, &polls))
	t.Cleanup(server.Close)
	base := oauth.Providers["chatgpt"]
	provider := base
	provider.ClientID = "client"
	provider.DeviceURL = server.URL + "/usercode"
	provider.DeviceTokenURL = server.URL + "/device-token"
	provider.TokenURL = server.URL + "/oauth-token"
	provider.DeviceRedirectURL = "https://example.test/device-callback"
	oauth.Providers["chatgpt"] = provider
	t.Cleanup(func() { oauth.Providers["chatgpt"] = base })
	home := t.TempDir()
	var stdout, stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDalgona, Home: home, Stdin: strings.NewReader(""), Stdout: &stdout, Stderr: &stderr}
	if code := RunCLI(t.Context(), env, []string{"login", "chatgpt", "--device-auth"}); code != 0 {
		t.Fatalf("device login exit %d: %s", code, stderr.String())
	}
	want := "Open https://auth.openai.com/codex/device and enter the code user-code to log in to chatgpt."
	if !strings.Contains(stdout.String(), want) || !strings.Contains(stdout.String(), "logged in to chatgpt") {
		t.Fatalf("stdout %q, want the device prompt and the success line", stdout.String())
	}
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	for _, entry := range store.Entries() {
		if entry.ID == "chatgpt" && entry.Credential.Type == "oauth" && entry.Credential.Access == "chatgpt-device-access" {
			return
		}
	}
	t.Fatal("device login did not store the ChatGPT OAuth credential")
}

func TestLoginDeviceAuthRejectsOtherProviders(t *testing.T) {
	var stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDalgona, Home: t.TempDir(), Stdout: &bytes.Buffer{}, Stderr: &stderr}
	if code := RunCLI(t.Context(), env, []string{"login", "anthropic", "--device-auth"}); code != 1 {
		t.Fatalf("anthropic --device-auth exit %d", code)
	}
	if !strings.Contains(stderr.String(), "login anthropic: --device-auth works only for chatgpt") {
		t.Fatalf("stderr %q", stderr.String())
	}
}

func TestLoginDeviceReportsMissingFlowFromDeviceCode(t *testing.T) {
	store, err := auth.Load(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	err = LoginDevice(t.Context(), store, "missing", &bytes.Buffer{})
	if err == nil || err.Error() != "login missing: provider has no device flow" {
		t.Fatalf("LoginDevice error = %v, want the device-code error", err)
	}
}

func chatGPTDeviceServer(t *testing.T, polls *atomic.Int32) http.Handler {
	t.Helper()
	return http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.Header().Set("Content-Type", "application/json")
		switch request.URL.Path {
		case "/usercode":
			deviceUserCode(t, writer, request)
		case "/device-token":
			deviceTokenPoll(writer, polls)
		case "/oauth-token":
			deviceCodeExchange(t, writer, request)
		default:
			writer.WriteHeader(http.StatusNotFound)
		}
	})
}

func deviceUserCode(t *testing.T, writer http.ResponseWriter, request *http.Request) {
	t.Helper()
	body, err := io.ReadAll(request.Body)
	if err != nil || !strings.Contains(string(body), `"client_id":"client"`) {
		t.Errorf("usercode body %s, error %v", body, err)
	}
	_, _ = io.WriteString(writer, `{"device_auth_id":"device-id","user_code":"user-code","interval":"0"}`)
}

func deviceTokenPoll(writer http.ResponseWriter, polls *atomic.Int32) {
	if polls.Add(1) == 1 {
		writer.WriteHeader(http.StatusNotFound)
		return
	}
	_, _ = io.WriteString(writer, `{"authorization_code":"code","code_challenge":"challenge","code_verifier":"verifier"}`)
}

func deviceCodeExchange(t *testing.T, writer http.ResponseWriter, request *http.Request) {
	t.Helper()
	if err := request.ParseForm(); err != nil {
		t.Errorf("parse code exchange: %v", err)
	}
	if request.Form.Get("code") != "code" || request.Form.Get("code_verifier") != "verifier" {
		t.Errorf("code exchange = %v", request.Form)
	}
	_, _ = io.WriteString(writer, `{"access_token":"chatgpt-device-access"}`)
}
