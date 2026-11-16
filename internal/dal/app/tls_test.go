package app

import (
	"bytes"
	"context"
	"encoding/pem"
	"fmt"
	"io"
	"net"
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

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/model"
	"github.com/gosuda/dalgona/internal/dal/model/chatgpt"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const unknownAuthorityText = "x509: certificate signed by unknown authority"

type privateServerHits struct {
	model, token, chat atomic.Int32
	failure            atomic.Pointer[error]
}

func TestTLSCAFileTrustsPrivateServer(t *testing.T) {
	hits := &privateServerHits{}
	server, caFile := newPrivateServer(t, http.HandlerFunc(hits.serve))

	base := oauth.Providers["chatgpt"]
	oauth.Providers["chatgpt"] = withTokenURL(base, server.URL+"/token")
	t.Cleanup(func() { oauth.Providers["chatgpt"] = base })

	newChat := func() *chatgpt.Provider {
		return chatgpt.New(chatgpt.Config{BaseURL: server.URL, Token: staticChatToken, Retry: instantRetry()})
	}

	requireUnknownAuthority(t, "model client", fetchPrivateModel(server.URL+"/model"))
	requireUnknownAuthority(t, "oauth refresh", refreshPrivateToken(t.Context()))
	_, chatControlErr := chatStreamErr(newChat())
	requireUnknownAuthority(t, "chat provider", chatControlErr)

	saved := http.DefaultTransport
	t.Cleanup(func() { http.DefaultTransport = saved })
	if err := InstallTLSRoots(config.Config{TLSCAFile: caFile}); err != nil {
		t.Fatal(err)
	}
	if http.DefaultTransport == saved {
		t.Fatal("InstallTLSRoots left http.DefaultTransport unchanged")
	}
	if err := InstallTLSRoots(config.Config{}); err != nil {
		t.Fatalf("InstallTLSRoots with an empty key: %v", err)
	}

	requirePrivateModelFetch(t, server.URL+"/model")
	requireRefreshedToken(t)
	requireChatCompletes(t, newChat())
	requirePrivateServerHits(t, hits)
}

func (h *privateServerHits) serve(w http.ResponseWriter, r *http.Request) {
	switch r.URL.Path {
	case "/model":
		h.model.Add(1)
		h.write(w, "ok")
	case "/token":
		h.token.Add(1)
		w.Header().Set("Content-Type", "application/json")
		h.write(w, `{"access_token":"fresh-access","refresh_token":"fresh-refresh","expires_in":3600}`)
	default:
		h.chat.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		h.write(w, "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n")
	}
}

func requirePrivateModelFetch(t *testing.T, url string) {
	t.Helper()
	response, err := model.NewHTTPClient(nil).Get(url)
	if err != nil {
		t.Fatalf("model client: %v", err)
	}
	body, readErr := io.ReadAll(response.Body)
	closeErr := response.Body.Close()
	if readErr != nil {
		t.Fatal(readErr)
	}
	if closeErr != nil {
		t.Fatal(closeErr)
	}
	if response.StatusCode != http.StatusOK || string(body) != "ok" {
		t.Fatalf("model response = %d %q", response.StatusCode, body)
	}
}

func requireRefreshedToken(t *testing.T) {
	t.Helper()
	token, err := oauth.Refresh(t.Context(), oauth.Providers["chatgpt"], "stale-refresh")
	if err != nil {
		t.Fatalf("oauth refresh: %v", err)
	}
	if token.Access != "fresh-access" {
		t.Fatalf("oauth access token = %q", token.Access)
	}
}

func requireChatCompletes(t *testing.T, provider *chatgpt.Provider) {
	t.Helper()
	stop, chatErr := chatStreamErr(provider)
	if chatErr != nil {
		t.Fatalf("chat provider: %v", chatErr)
	}
	if stop != "stop" {
		t.Fatalf("chat stop reason = %q, want stop", stop)
	}
}

func requirePrivateServerHits(t *testing.T, hits *privateServerHits) {
	t.Helper()
	if err := hits.err(); err != nil {
		t.Fatal(err)
	}
	for name, count := range map[string]int32{
		"model": hits.model.Load(), "token": hits.token.Load(), "chat": hits.chat.Load(),
	} {
		if count == 0 {
			t.Fatalf("no %s request reached the private server", name)
		}
	}
}

func staticChatToken(context.Context) (oauth.Token, error) {
	return oauth.Token{Access: "test-access", Account: "acct-1"}, nil
}

func instantRetry() model.RetryOptions {
	return model.RetryOptions{
		StartTimeout: time.Second,
		Sleep:        func(context.Context, time.Duration) error { return nil },
		Random:       func() float64 { return 0.5 },
	}
}

func requireUnknownAuthority(t *testing.T, name string, err error) {
	t.Helper()
	if err == nil || !strings.Contains(err.Error(), unknownAuthorityText) {
		t.Fatalf("%s error = %v, want text %q", name, err, unknownAuthorityText)
	}
}

func fetchPrivateModel(url string) error {
	response, err := model.NewHTTPClient(nil).Get(url)
	if err != nil {
		return err
	}
	_, readErr := io.Copy(io.Discard, response.Body)
	closeErr := response.Body.Close()
	if readErr != nil {
		return readErr
	}
	return closeErr
}

func refreshPrivateToken(ctx context.Context) error {
	_, err := oauth.Refresh(ctx, oauth.Providers["chatgpt"], "stale-refresh")
	return err
}

func chatStreamErr(provider *chatgpt.Provider) (string, error) {
	stream, err := provider.Stream(context.Background(), model.Request{Model: "gpt-6.1-sol"})
	if err != nil {
		return "", err
	}
	stop, failure := "", error(nil)
	for event := range stream.Events() {
		switch value := event.(type) {
		case model.Done:
			stop = value.StopReason
		case model.Error:
			failure = value.Err
		}
	}
	return stop, failure
}

func (h *privateServerHits) write(w io.Writer, text string) {
	if _, err := io.WriteString(w, text); err != nil {
		failure := fmt.Errorf("test server write: %w", err)
		h.failure.CompareAndSwap(nil, &failure)
	}
}

func (h *privateServerHits) err() error {
	if stored := h.failure.Load(); stored != nil {
		return *stored
	}
	return nil
}

func newPrivateServer(t *testing.T, handler http.Handler) (*httptest.Server, string) {
	t.Helper()
	server := httptest.NewTLSServer(handler)
	t.Cleanup(server.Close)
	caFile := filepath.Join(t.TempDir(), "ca.pem")
	certificate := pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: server.Certificate().Raw})
	if err := os.WriteFile(caFile, certificate, 0o600); err != nil {
		t.Fatal(err)
	}
	return server, caFile
}

func restoreDefaultTransport(t *testing.T) {
	t.Helper()
	saved := http.DefaultTransport
	t.Cleanup(func() { http.DefaultTransport = saved })
}

type commandOutput struct {
	code           int
	stdout, stderr string
}

func trustPrivateCA(caFile string) []string {
	return []string{fmt.Sprintf("tls_ca_file = %q", caFile)}
}

func omitPrivateCA(string) []string {
	return nil
}

func privateProductHome(t *testing.T, topLevel []string, tables ...string) string {
	t.Helper()
	home := t.TempDir()
	t.Setenv("DAL_HOME", home)
	text := strings.Join(slices.Concat(topLevel, tables), "\n") + "\n"
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte(text), 0o600); err != nil {
		t.Fatal(err)
	}
	return home
}

func useDeviceEndpoints(t *testing.T, base string) {
	t.Helper()
	original := oauth.Providers["chatgpt"]
	provider := original
	provider.ClientID = "client"
	provider.DeviceURL = base + "/usercode"
	provider.DeviceTokenURL = base + "/device-token"
	provider.TokenURL = base + "/oauth-token"
	provider.DeviceRedirectURL = "https://example.test/device-callback"
	oauth.Providers["chatgpt"] = provider
	t.Cleanup(func() { oauth.Providers["chatgpt"] = original })
}

func runPrivateDeviceLogin(t *testing.T, setting func(caFile string) []string) (commandOutput, bool) {
	t.Helper()
	restoreDefaultTransport(t)
	var polls atomic.Int32
	server, caFile := newPrivateServer(t, chatGPTDeviceServer(t, &polls))
	useDeviceEndpoints(t, server.URL)
	home := privateProductHome(t, setting(caFile))
	var stdout, stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDal, Home: home, Stdin: strings.NewReader(""), Stdout: &stdout, Stderr: &stderr}
	code := RunCLI(t.Context(), env, []string{"login", "chatgpt", "--device-auth"})
	return commandOutput{code: code, stdout: stdout.String(), stderr: stderr.String()}, storedDeviceCredential(t, home)
}

func storedDeviceCredential(t *testing.T, home string) bool {
	t.Helper()
	for _, entry := range loadStore(t, home).Entries() {
		if entry.ID == "chatgpt" && entry.Credential.Access == "chatgpt-device-access" {
			return true
		}
	}
	return false
}

func TestLoginDeviceTrustsTLSCAFile(t *testing.T) {
	out, stored := runPrivateDeviceLogin(t, trustPrivateCA)
	if out.code != 0 || !stored {
		t.Fatalf("device login exit %d, credential stored %t, stderr %q, want exit 0 and a stored credential", out.code, stored, out.stderr)
	}
}

func TestLoginDeviceFailsWithoutTLSCAFile(t *testing.T) {
	out, stored := runPrivateDeviceLogin(t, omitPrivateCA)
	if out.code != 1 || stored || !strings.Contains(out.stderr, unknownAuthorityText) {
		t.Fatalf("device login exit %d, credential stored %t, stderr %q, want exit 1, no credential and %q", out.code, stored, out.stderr, unknownAuthorityText)
	}
}

func serveOpenAIModels(t *testing.T) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/models" || r.Header.Get("Authorization") != "Bearer listed-key" {
			http.Error(w, "unexpected request", http.StatusUnauthorized)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		if _, err := io.WriteString(w, `{"data":[{"id":"gpt-test"}]}`); err != nil {
			t.Errorf("models server write: %v", err)
		}
	}
}

func dialHost(t *testing.T, host *agent.Host) func(context.Context) (io.ReadWriteCloser, error) {
	t.Helper()
	var served sync.WaitGroup
	t.Cleanup(served.Wait)
	return func(ctx context.Context) (io.ReadWriteCloser, error) {
		client, server := net.Pipe()
		served.Go(func() { _ = rpc.ServeWith(context.WithoutCancel(ctx), server, host, rpc.ServeOptions{}) })
		return client, nil
	}
}

func runPrivateModels(t *testing.T, setting func(caFile string) []string) commandOutput {
	t.Helper()
	restoreDefaultTransport(t)
	clearProviderKeys(t)
	server, caFile := newPrivateServer(t, serveOpenAIModels(t))
	home := privateProductHome(t, setting(caFile), "[providers.openai]", fmt.Sprintf("base_url = %q", server.URL))
	seedAPIKey(t, home, "openai", "listed-key")
	host, closeHost, err := NewHost(t.Context(), Options{Product: config.ProductDal, Cwd: t.TempDir()})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := closeHost(); err != nil {
			t.Errorf("close host: %v", err)
		}
	})
	var stdout, stderr bytes.Buffer
	env := CLIEnv{Product: config.ProductDal, Home: home, Dial: dialHost(t, host), Stdout: &stdout, Stderr: &stderr}
	code := RunCLI(t.Context(), env, []string{"models"})
	return commandOutput{code: code, stdout: stdout.String(), stderr: stderr.String()}
}

func TestModelsTrustsTLSCAFile(t *testing.T) {
	out := runPrivateModels(t, trustPrivateCA)
	if out.code != 0 || !strings.Contains(out.stdout, "openai/gpt-test") {
		t.Fatalf("models exit %d, stdout %q, stderr %q, want exit 0 and openai/gpt-test", out.code, out.stdout, out.stderr)
	}
}

func TestModelsFailsWithoutTLSCAFile(t *testing.T) {
	out := runPrivateModels(t, omitPrivateCA)
	if out.code != 1 || out.stdout != "" || !strings.Contains(out.stderr, unknownAuthorityText) {
		t.Fatalf("models exit %d, stdout %q, stderr %q, want exit 1, no output and %q", out.code, out.stdout, out.stderr, unknownAuthorityText)
	}
}
