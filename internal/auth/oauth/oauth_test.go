package oauth_test

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/auth/oauth"
)

type callbackPage struct {
	body string
	err  error
}

func TestPKCELoopbackRoundTrip(t *testing.T) {
	var tokenCalls atomic.Int32
	var expectedChallenge atomic.Value
	tokenServer := scopedServer(t, loopbackTokenHandler(t, &tokenCalls, &expectedChallenge))
	defer tokenServer.Close()
	provider := testProvider(tokenServer.URL, freePort(t), 0)
	page := make(chan callbackPage, 1)
	open := func(rawURL string) error {
		return openLoopback(rawURL, &expectedChallenge, page)
	}
	before := time.Now()
	token, err := oauth.PKCE(context.Background(), provider, open, nil)
	if err != nil {
		t.Fatalf("PKCE: %v", err)
	}
	if token.Access != "access" || token.Refresh != "refresh" {
		t.Fatalf("unexpected token: %+v", token)
	}
	if !token.Expires.After(before.Add(3500 * time.Second)) {
		t.Fatalf("expiry did not use expires_in: %v", token.Expires)
	}
	result := <-page
	if result.err != nil {
		t.Fatalf("callback: %v", result.err)
	}
	if result.body != "Login complete. You can close this tab." {
		t.Fatalf("callback body %q", result.body)
	}
	if tokenCalls.Load() != 1 {
		t.Fatalf("token calls = %d", tokenCalls.Load())
	}
}

func loopbackTokenHandler(t *testing.T, calls *atomic.Int32, expected *atomic.Value) http.HandlerFunc {
	t.Helper()
	return func(writer http.ResponseWriter, request *http.Request) {
		calls.Add(1)
		if err := request.ParseForm(); err != nil {
			t.Errorf("parse token form: %v", err)
			writer.WriteHeader(http.StatusBadRequest)
			return
		}
		verifier := request.Form.Get("code_verifier")
		digest := sha256.Sum256([]byte(verifier))
		challenge := base64.RawURLEncoding.EncodeToString(digest[:])
		if challenge != expected.Load() {
			t.Errorf("challenge was not independently derived from verifier")
		}
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"access_token":"access","refresh_token":"refresh","expires_in":3600}`)
	}
}

func openLoopback(rawURL string, expected *atomic.Value, page chan<- callbackPage) error {
	authURL, err := url.Parse(rawURL)
	if err != nil {
		return err
	}
	query := authURL.Query()
	challenge := query.Get("code_challenge")
	state := query.Get("state")
	expected.Store(challenge)
	if query.Get("code_challenge_method") != "S256" {
		return errors.New("PKCE method is not S256")
	}
	redirect := query.Get("redirect_uri")
	go requestPage(redirect+"?code=browser-code&state="+url.QueryEscape(state), page)
	return nil
}

func TestPKCEStateMismatch(t *testing.T) {
	var tokenCalls atomic.Int32
	tokenServer := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		tokenCalls.Add(1)
		writer.WriteHeader(http.StatusInternalServerError)
	}))
	defer tokenServer.Close()
	provider := testProvider(tokenServer.URL, freePort(t), 0)
	open := func(rawURL string) error {
		authURL, err := url.Parse(rawURL)
		if err != nil {
			return err
		}
		redirect := authURL.Query().Get("redirect_uri")
		go requestPage(redirect+"?code=code&state=wrong", make(chan callbackPage, 1))
		return nil
	}
	_, err := oauth.PKCE(context.Background(), provider, open, nil)
	if err == nil || err.Error() != "OAuth state mismatch" {
		t.Fatalf("state mismatch error = %v", err)
	}
	if tokenCalls.Load() != 0 {
		t.Fatalf("mismatched state reached token endpoint")
	}
}

func TestPKCEPasteInput(t *testing.T) {
	t.Run("full redirect URL", func(t *testing.T) { runPKCEPasteCase(t, pastedFullURL) })
	t.Run("bare code", func(t *testing.T) { runPKCEPasteCase(t, pastedBareCode) })
}

func runPKCEPasteCase(t *testing.T, input func(*url.URL) string) {
	t.Helper()
	tokenServer := scopedServer(t, pasteTokenHandler(t))
	defer tokenServer.Close()
	provider := testProvider(tokenServer.URL, freePort(t), 0)
	var pasted string
	open := func(rawURL string) error {
		authURL, err := url.Parse(rawURL)
		if err != nil {
			return err
		}
		pasted = input(authURL)
		return nil
	}
	paste := func(context.Context) (string, error) { return pasted, nil }
	token, err := oauth.PKCE(context.Background(), provider, open, paste)
	if err != nil {
		t.Fatalf("PKCE paste: %v", err)
	}
	if token.Access != "pasted-access" {
		t.Fatalf("access = %q", token.Access)
	}
}

func pasteTokenHandler(t *testing.T) http.HandlerFunc {
	t.Helper()
	return func(writer http.ResponseWriter, request *http.Request) {
		if err := request.ParseForm(); err != nil {
			t.Errorf("parse form: %v", err)
		}
		if request.Form.Get("code") != "pasted-code" {
			t.Errorf("code = %q", request.Form.Get("code"))
		}
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"access_token":"pasted-access"}`)
	}
}

func pastedFullURL(authURL *url.URL) string {
	return "http://localhost/callback?code=pasted-code&state=" + url.QueryEscape(authURL.Query().Get("state"))
}

func pastedBareCode(*url.URL) string {
	return "pasted-code"
}

func TestPKCEBusyPrimaryPortUsesFallback(t *testing.T) {
	primary := freePort(t)
	listener, listenErr := net.Listen("tcp", net.JoinHostPort("127.0.0.1", fmt.Sprint(primary)))
	if listenErr != nil {
		t.Fatalf("occupy primary port: %v", listenErr)
	}
	defer listener.Close()
	fallback := freePort(t)
	tokenServer := scopedServer(t, fallbackTokenHandler(t, fallback))
	defer tokenServer.Close()
	provider := testProvider(tokenServer.URL, primary, fallback)
	page := make(chan callbackPage, 1)
	open := func(rawURL string) error { return openFallback(rawURL, fallback, page) }
	token, pkceErr := oauth.PKCE(context.Background(), provider, open, nil)
	if pkceErr != nil {
		t.Fatalf("PKCE fallback: %v", pkceErr)
	}
	if token.Access != "fallback" {
		t.Fatalf("access = %q", token.Access)
	}
	if result := <-page; result.err != nil {
		t.Fatalf("fallback callback: %v", result.err)
	}
}

func fallbackTokenHandler(t *testing.T, fallback int) http.HandlerFunc {
	t.Helper()
	return func(writer http.ResponseWriter, request *http.Request) {
		if err := request.ParseForm(); err != nil {
			t.Errorf("parse form: %v", err)
		}
		if !strings.HasSuffix(request.Form.Get("redirect_uri"), fmt.Sprintf(":%d/callback", fallback)) {
			t.Errorf("token redirect = %q", request.Form.Get("redirect_uri"))
		}
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"access_token":"fallback"}`)
	}
}

func openFallback(rawURL string, fallback int, page chan<- callbackPage) error {
	authURL, parseErr := url.Parse(rawURL)
	if parseErr != nil {
		return parseErr
	}
	query := authURL.Query()
	redirect, redirectErr := url.Parse(query.Get("redirect_uri"))
	if redirectErr != nil {
		return redirectErr
	}
	if redirect.Port() != fmt.Sprint(fallback) {
		return fmt.Errorf("redirect port = %s, want %d", redirect.Port(), fallback)
	}
	go requestPage(query.Get("redirect_uri")+"?code=code&state="+url.QueryEscape(query.Get("state")), page)
	return nil
}

func TestRefresh(t *testing.T) {
	server := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if err := request.ParseForm(); err != nil {
			t.Errorf("parse refresh form: %v", err)
		}
		if request.Form.Get("grant_type") != "refresh_token" || request.Form.Get("refresh_token") != "old-refresh" {
			t.Errorf("refresh form = %v", request.Form)
		}
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"access_token":"new-access","expires_in":60}`)
	}))
	defer server.Close()
	token, err := oauth.Refresh(context.Background(), oauth.Provider{ID: "test", TokenURL: server.URL}, "old-refresh")
	if err != nil {
		t.Fatalf("refresh: %v", err)
	}
	if token.Access != "new-access" || token.Refresh != "old-refresh" {
		t.Fatalf("refresh result = %+v", token)
	}
}

func TestChatGPTRefreshReadsAccountClaim(t *testing.T) {
	server := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		body, err := io.ReadAll(request.Body)
		if err != nil {
			t.Errorf("read JSON refresh: %v", err)
		}
		if !strings.Contains(string(body), `"grant_type":"refresh_token"`) {
			t.Errorf("JSON refresh body = %s", body)
		}
		payload := base64.RawURLEncoding.EncodeToString([]byte(`{"https://api.openai.com/auth":{"chatgpt_account_id":"account-123"}}`))
		writer.Header().Set("Content-Type", "application/json")
		_, _ = fmt.Fprintf(writer, `{"access_token":"new-access","id_token":"x.%s.x"}`, payload)
	}))
	defer server.Close()
	token, err := oauth.Refresh(context.Background(), oauth.Provider{ID: "chatgpt", TokenURL: server.URL}, "old")
	if err != nil {
		t.Fatalf("ChatGPT refresh: %v", err)
	}
	if token.Account != "account-123" {
		t.Fatalf("account = %q", token.Account)
	}
}

func testProvider(tokenURL string, primary, fallback int) oauth.Provider {
	return oauth.Provider{
		ID: "test", AuthorizeURL: "https://example.test/authorize", TokenURL: tokenURL,
		ClientID: "client", RedirectHost: "127.0.0.1", RedirectPort: primary,
		FallbackPort: fallback, RedirectPath: "/callback",
	}
}

func freePort(t *testing.T) int {
	t.Helper()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("allocate port: %v", err)
	}
	port := listener.Addr().(*net.TCPAddr).Port
	if err := listener.Close(); err != nil {
		t.Fatalf("release port: %v", err)
	}
	return port
}

func requestPage(endpoint string, result chan<- callbackPage) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	request, requestErr := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if requestErr != nil {
		result <- callbackPage{err: requestErr}
		return
	}
	client := http.Client{Timeout: 5 * time.Second}
	response, err := client.Do(request)
	if err != nil {
		result <- callbackPage{err: err}
		return
	}
	body, readErr := io.ReadAll(response.Body)
	closeErr := response.Body.Close()
	result <- callbackPage{body: string(body), err: errors.Join(readErr, closeErr)}
}
