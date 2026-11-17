package mcp_test

import (
	"encoding/base64"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/mcp"
)

var emailIDToken = "e30." + base64.RawURLEncoding.EncodeToString([]byte(`{"email":"user@example.test"}`)) + ".sig"

type authServer struct {
	t       *testing.T
	accept  func(token string) bool
	mu      sync.Mutex
	posts   []string
	deleted string
}

func newAuthServer(t *testing.T, accept func(token string) bool) (*httptest.Server, *authServer) {
	t.Helper()
	fixture := &authServer{t: t, accept: accept}
	mux := http.NewServeMux()
	mux.HandleFunc("POST /", fixture.handle)
	mux.HandleFunc("DELETE /", fixture.handleDelete)
	server := httptest.NewServer(mux)
	t.Cleanup(server.Close)
	return server, fixture
}

func (s *authServer) handle(writer http.ResponseWriter, request *http.Request) {
	header := request.Header.Get("Authorization")
	s.mu.Lock()
	s.posts = append(s.posts, header)
	s.mu.Unlock()
	token, found := strings.CutPrefix(header, "Bearer ")
	if !found || !s.accept(token) {
		writer.Header().Set("WWW-Authenticate", `Bearer realm="mcp"`)
		writer.WriteHeader(http.StatusUnauthorized)
		return
	}
	message, err := decodeRequest(request.Body)
	if err != nil {
		writer.WriteHeader(http.StatusBadRequest)
		return
	}
	switch message.Method {
	case "initialize":
		writer.Header().Set("Mcp-Session-Id", "session-auth")
		writeHTTPJSON(s.t, writer, message.ID, initResult)
	case "notifications/initialized":
		writer.WriteHeader(http.StatusAccepted)
	case "tools/list":
		writeHTTPJSON(s.t, writer, message.ID, echoListResult)
	default:
		writer.WriteHeader(http.StatusBadRequest)
	}
}

func (s *authServer) handleDelete(writer http.ResponseWriter, request *http.Request) {
	s.mu.Lock()
	s.deleted = request.Header.Get("Authorization")
	s.mu.Unlock()
	writer.WriteHeader(http.StatusOK)
}

func (s *authServer) authorizations() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Clone(s.posts)
}

func (s *authServer) deleteAuthorization() string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.deleted
}

type tokenEndpoint struct {
	t      *testing.T
	status int
	grants atomic.Int32
	mu     sync.Mutex
	forms  []url.Values
}

func newTokenEndpoint(t *testing.T, status int) (*httptest.Server, *tokenEndpoint) {
	t.Helper()
	endpoint := &tokenEndpoint{t: t, status: status}
	server := httptest.NewServer(http.HandlerFunc(endpoint.handle))
	t.Cleanup(server.Close)
	return server, endpoint
}

func (e *tokenEndpoint) handle(writer http.ResponseWriter, request *http.Request) {
	e.grants.Add(1)
	if err := request.ParseForm(); err != nil {
		e.t.Errorf("parse token form: %v", err)
		writer.WriteHeader(http.StatusBadRequest)
		return
	}
	e.mu.Lock()
	e.forms = append(e.forms, request.PostForm)
	e.mu.Unlock()
	if e.status != http.StatusOK {
		writer.WriteHeader(e.status)
		_, _ = io.WriteString(writer, `{"error":"invalid_grant"}`)
		return
	}
	writer.Header().Set("Content-Type", "application/json")
	_, _ = io.WriteString(writer, `{"access_token":"fresh-access","refresh_token":"new-refresh","expires_in":3600,"id_token":"`+emailIDToken+`"}`)
}

func (e *tokenEndpoint) firstForm() url.Values {
	e.mu.Lock()
	defer e.mu.Unlock()
	if len(e.forms) == 0 {
		return url.Values{}
	}
	return e.forms[0]
}

type oauthEnv struct {
	options  mcp.AuthOptions
	store    *auth.Store
	resource string
}

func newOAuthEnv(t *testing.T, mcpURL string) oauthEnv {
	t.Helper()
	home := t.TempDir()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatalf("load auth store: %v", err)
	}
	resource, err := oauth.CanonicalResource(mcpURL)
	if err != nil {
		t.Fatalf("canonical resource: %v", err)
	}
	options := mcp.AuthOptions{Name: "web", URL: mcpURL, OAuth: true, Home: home, Store: store}
	return oauthEnv{options: options, store: store, resource: resource}
}

func (e oauthEnv) writeRegistration(t *testing.T, resource, tokenBase, clientID string) {
	t.Helper()
	body := fmt.Sprintf(`{"web":{"resource":%q,"issuer":%q,"authorization_endpoint":%q,"token_endpoint":%q,"client_id":%q,"scopes":["read"]}}`,
		resource, tokenBase, tokenBase+"/authorize", tokenBase+"/token", clientID)
	if err := os.WriteFile(filepath.Join(e.options.Home, mcp.RegistrationFile), []byte(body), 0o600); err != nil {
		t.Fatalf("write registration: %v", err)
	}
}

func (e oauthEnv) setCredential(t *testing.T, account, access string) {
	t.Helper()
	credential := auth.Credential{
		Type: "oauth", Access: access, Refresh: "old-refresh", Account: account,
		Expires: time.Now().Add(time.Hour).UTC().Format(time.RFC3339),
	}
	if err := e.store.SetCredential(mcp.CredentialKey("web"), credential); err != nil {
		t.Fatalf("store credential: %v", err)
	}
}

func (e oauthEnv) storedCredential(t *testing.T) auth.Credential {
	t.Helper()
	for _, entry := range e.store.Entries() {
		if entry.ID == mcp.CredentialKey("web") {
			return entry.Credential
		}
	}
	t.Fatal("no stored mcp-web credential")
	return auth.Credential{}
}

func connectWith(t *testing.T, serverURL string, options mcp.AuthOptions) (*mcp.Client, error) {
	t.Helper()
	source, err := mcp.NewTokenSource(options)
	if err != nil {
		t.Fatalf("NewTokenSource: %v", err)
	}
	return mcp.New(t.Context(), mcp.Config{Name: options.Name, URL: serverURL, Auth: source})
}

func acceptOnly(tokens ...string) func(string) bool {
	return func(token string) bool { return slices.Contains(tokens, token) }
}

func TestUnauthorizedRefreshesOnce(t *testing.T) {
	t.Run("refresh succeeds and the retry carries the new token", func(t *testing.T) { checkRefreshThenRetry(t) })
	t.Run("a second 401 asks for login and the refresh count stays 1", func(t *testing.T) { checkSecondUnauthorized(t) })
	t.Run("a failed refresh asks for login without a retry", func(t *testing.T) { checkFailedRefresh(t) })
}

func checkRefreshThenRetry(t *testing.T) {
	t.Helper()
	server, mcpServer := newAuthServer(t, acceptOnly("fresh-access"))
	tokenServer, endpoint := newTokenEndpoint(t, http.StatusOK)
	mcpURL := server.URL + "/mcp"
	env := newOAuthEnv(t, mcpURL)
	env.writeRegistration(t, env.resource, tokenServer.URL, "client-1")
	env.setCredential(t, env.resource, "stale-access")
	client, err := connectWith(t, mcpURL, env.options)
	if err != nil {
		t.Fatalf("connect after a 401: %v", err)
	}
	if _, err := client.Tools(t.Context()); err != nil {
		t.Fatalf("list tools: %v", err)
	}
	if err := client.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	checkRefreshGrantForm(t, endpoint, env)
	checkRefreshAuthorization(t, mcpServer)
	checkStoredCredential(t, env)
}

func checkRefreshGrantForm(t *testing.T, endpoint *tokenEndpoint, env oauthEnv) {
	t.Helper()
	if got := endpoint.grants.Load(); got != 1 {
		t.Fatalf("refresh grants = %d, want 1", got)
	}
	form := endpoint.firstForm()
	wantForm := map[string]string{"grant_type": "refresh_token", "refresh_token": "old-refresh", "client_id": "client-1", "resource": env.resource}
	for key, want := range wantForm {
		if got := form.Get(key); got != want {
			t.Errorf("refresh form %s = %q, want %q", key, got, want)
		}
	}
}

func checkRefreshAuthorization(t *testing.T, mcpServer *authServer) {
	t.Helper()
	headers := mcpServer.authorizations()
	if len(headers) < 3 || headers[0] != "Bearer stale-access" || slices.Contains(headers[1:], "Bearer stale-access") {
		t.Fatalf("authorization sequence = %v, want one stale request then only fresh ones", headers)
	}
	if got := mcpServer.deleteAuthorization(); got != "Bearer fresh-access" {
		t.Errorf("session DELETE authorization = %q, want the fresh token", got)
	}
}

func checkStoredCredential(t *testing.T, env oauthEnv) {
	t.Helper()
	stored := env.storedCredential(t)
	if stored.Access != "fresh-access" || stored.Refresh != "new-refresh" || stored.Account != env.resource {
		t.Fatalf("stored credential = %+v, want the rotated tokens bound to %s", stored, env.resource)
	}
}

func checkSecondUnauthorized(t *testing.T) {
	t.Helper()
	server, mcpServer := newAuthServer(t, acceptOnly())
	tokenServer, endpoint := newTokenEndpoint(t, http.StatusOK)
	mcpURL := server.URL + "/mcp"
	env := newOAuthEnv(t, mcpURL)
	env.writeRegistration(t, env.resource, tokenServer.URL, "client-1")
	env.setCredential(t, env.resource, "stale-access")
	_, err := connectWith(t, mcpURL, env.options)
	if login, isLogin := errors.AsType[*mcp.LoginRequiredError](err); !isLogin || login.Name != "web" {
		t.Fatalf("error = %v, want *LoginRequiredError for web", err)
	}
	if !strings.Contains(err.Error(), "MCP server web needs login: run dalgona mcp login web") {
		t.Fatalf("error text = %q", err)
	}
	if got := endpoint.grants.Load(); got != 1 {
		t.Fatalf("refresh grants = %d, want 1", got)
	}
	if got := len(mcpServer.authorizations()); got != 2 {
		t.Fatalf("MCP requests = %d, want the original and one retry", got)
	}
}

func checkFailedRefresh(t *testing.T) {
	t.Helper()
	server, mcpServer := newAuthServer(t, acceptOnly("fresh-access"))
	tokenServer, endpoint := newTokenEndpoint(t, http.StatusBadRequest)
	mcpURL := server.URL + "/mcp"
	env := newOAuthEnv(t, mcpURL)
	env.writeRegistration(t, env.resource, tokenServer.URL, "client-1")
	env.setCredential(t, env.resource, "stale-access")
	_, err := connectWith(t, mcpURL, env.options)
	if login, isLogin := errors.AsType[*mcp.LoginRequiredError](err); !isLogin || login.Err == nil {
		t.Fatalf("error = %v, want *LoginRequiredError with the refresh failure as cause", err)
	}
	if got := endpoint.grants.Load(); got != 1 {
		t.Fatalf("refresh grants = %d, want 1", got)
	}
	if got := len(mcpServer.authorizations()); got != 1 {
		t.Fatalf("MCP requests = %d, want 1 and no retry", got)
	}
}

func TestBearerTokenEnv(t *testing.T) {
	const name = "MCP_BEARER_TOKEN_TEST"
	options := func(serverURL string) mcp.AuthOptions {
		return mcp.AuthOptions{Name: "web", URL: serverURL, BearerTokenEnv: name}
	}
	t.Run("header carries the variable and is read on every call", func(t *testing.T) {
		bearerEnvRotates(t, name, options)
	})
	t.Run("unset variable fails before any request", func(t *testing.T) {
		bearerEnvUnset(t, name, options)
	})
	t.Run("rejected token is reported without a retry", func(t *testing.T) {
		bearerEnvRejected(t, name, options)
	})
}

func bearerEnvRotates(t *testing.T, name string, options func(string) mcp.AuthOptions) {
	t.Helper()
	server, fixture := newAuthServer(t, acceptOnly("secret-1", "secret-2"))
	t.Setenv(name, "secret-1")
	client, err := connectWith(t, server.URL, options(server.URL))
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	t.Setenv(name, "secret-2")
	if _, err := client.Tools(t.Context()); err != nil {
		t.Fatalf("list tools: %v", err)
	}
	if err := client.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	headers := fixture.authorizations()
	if len(headers) < 3 || headers[0] != "Bearer secret-1" || headers[len(headers)-1] != "Bearer secret-2" {
		t.Fatalf("authorization sequence = %v, want secret-1 first and secret-2 last", headers)
	}
	if got := mcp.AuthStatus(options(server.URL)); got != "bearerToken" {
		t.Fatalf("status = %q, want bearerToken", got)
	}
}

func bearerEnvUnset(t *testing.T, name string, options func(string) mcp.AuthOptions) {
	t.Helper()
	server, fixture := newAuthServer(t, acceptOnly("secret-1"))
	t.Setenv(name, "")
	_, err := connectWith(t, server.URL, options(server.URL))
	if err == nil || !strings.Contains(err.Error(), "bearer_token_env "+name+" is not set") {
		t.Fatalf("error = %v, want the unset message", err)
	}
	if got := len(fixture.authorizations()); got != 0 {
		t.Fatalf("server saw %d requests, want 0", got)
	}
	if got := mcp.AuthStatus(options(server.URL)); got != "notLoggedIn" {
		t.Fatalf("status = %q, want notLoggedIn", got)
	}
}

func bearerEnvRejected(t *testing.T, name string, options func(string) mcp.AuthOptions) {
	t.Helper()
	server, fixture := newAuthServer(t, acceptOnly("secret-1"))
	t.Setenv(name, "wrong")
	_, err := connectWith(t, server.URL, options(server.URL))
	if _, isLogin := errors.AsType[*mcp.LoginRequiredError](err); err == nil || isLogin || !strings.Contains(err.Error(), "rejected the token in bearer_token_env "+name) {
		t.Fatalf("error = %v, want a bearer_token_env rejection and no login request", err)
	}
	if got := len(fixture.authorizations()); got != 1 {
		t.Fatalf("server saw %d requests, want 1", got)
	}
}

func TestTokenBoundToResource(t *testing.T) {
	t.Run("token for another resource is no credential", func(t *testing.T) { checkForeignAccount(t) })
	t.Run("bound token connects and ignores provider environment", func(t *testing.T) { checkBoundToken(t) })
	t.Run("registration for another resource or client is refused", func(t *testing.T) { checkRegistrationBinding(t) })
	t.Run("missing registration asks for login", func(t *testing.T) { checkMissingRegistration(t) })
}

func checkForeignAccount(t *testing.T) {
	t.Helper()
	server, fixture := newAuthServer(t, acceptOnly("stored-access"))
	tokenServer, endpoint := newTokenEndpoint(t, http.StatusOK)
	mcpURL := server.URL + "/mcp"
	env := newOAuthEnv(t, mcpURL)
	env.writeRegistration(t, env.resource, tokenServer.URL, "client-1")
	env.setCredential(t, "http://other.example.test/mcp", "stored-access")
	if got := mcp.AuthStatus(env.options); got != "notLoggedIn" {
		t.Fatalf("status = %q, want notLoggedIn", got)
	}
	_, err := connectWith(t, mcpURL, env.options)
	want := "mcp-web token was issued for http://other.example.test/mcp, not " + env.resource + "; run dalgona mcp login web"
	if err == nil || !strings.Contains(err.Error(), want) {
		t.Fatalf("error = %v, want it to contain %q", err, want)
	}
	if requests, grants := len(fixture.authorizations()), endpoint.grants.Load(); requests != 0 || grants != 0 {
		t.Fatalf("requests = %d, grants = %d, want none before the credential is bound", requests, grants)
	}
	env.setCredential(t, env.resource, "stored-access")
	if got := mcp.AuthStatus(env.options); got != "oAuth" {
		t.Fatalf("status after a bound login = %q, want oAuth", got)
	}
	client, err := connectWith(t, mcpURL, env.options)
	if err != nil {
		t.Fatalf("connect with the bound token: %v", err)
	}
	if err := client.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
}

func checkBoundToken(t *testing.T) {
	t.Helper()
	server, fixture := newAuthServer(t, acceptOnly("stored-access"))
	tokenServer, _ := newTokenEndpoint(t, http.StatusOK)
	mcpURL := server.URL + "/mcp"
	env := newOAuthEnv(t, mcpURL)
	env.writeRegistration(t, env.resource, tokenServer.URL, "client-1")
	env.setCredential(t, env.resource, "stored-access")
	t.Setenv("MCP_WEB_API_KEY", "env-key")
	client, err := connectWith(t, mcpURL, env.options)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	if err := client.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	for _, header := range fixture.authorizations() {
		if header != "Bearer stored-access" {
			t.Fatalf("authorization = %q, want the stored token and never the environment key", header)
		}
	}
}

func checkRegistrationBinding(t *testing.T) {
	t.Helper()
	server, fixture := newAuthServer(t, acceptOnly("stored-access"))
	tokenServer, _ := newTokenEndpoint(t, http.StatusOK)
	mcpURL := server.URL + "/mcp"
	env := newOAuthEnv(t, mcpURL)
	env.setCredential(t, env.resource, "stored-access")
	env.writeRegistration(t, "http://other.example.test/mcp", tokenServer.URL, "client-1")
	_, err := connectWith(t, mcpURL, env.options)
	if err == nil || !strings.Contains(err.Error(), "mcp-oauth.json registration is for http://other.example.test/mcp, not "+env.resource) {
		t.Fatalf("resource mismatch error = %v", err)
	}
	env.writeRegistration(t, env.resource, tokenServer.URL, "client-1")
	env.options.OAuthClientID = "client-2"
	_, err = connectWith(t, mcpURL, env.options)
	if err == nil || !strings.Contains(err.Error(), "registration uses client client-1, not oauth_client_id client-2") {
		t.Fatalf("client mismatch error = %v", err)
	}
	if got := len(fixture.authorizations()); got != 0 {
		t.Fatalf("server saw %d requests with a mismatched registration, want 0", got)
	}
}

func checkMissingRegistration(t *testing.T) {
	t.Helper()
	server, fixture := newAuthServer(t, acceptOnly("stored-access"))
	mcpURL := server.URL + "/mcp"
	env := newOAuthEnv(t, mcpURL)
	env.setCredential(t, env.resource, "stored-access")
	if got := mcp.AuthStatus(env.options); got != "notLoggedIn" {
		t.Fatalf("status = %q, want notLoggedIn", got)
	}
	_, err := connectWith(t, mcpURL, env.options)
	if _, isLogin := errors.AsType[*mcp.LoginRequiredError](err); !isLogin {
		t.Fatalf("error = %v, want *LoginRequiredError", err)
	}
	if got := len(fixture.authorizations()); got != 0 {
		t.Fatalf("server saw %d requests, want 0", got)
	}
}

func TestAuthorizationStaysOnItsOrigin(t *testing.T) {
	const name = "MCP_BEARER_REDIRECT_TEST"
	foreign := make(chan string, 1)
	foreignServer := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		foreign <- request.Header.Get("Authorization")
		writer.WriteHeader(http.StatusBadRequest)
	}))
	defer foreignServer.Close()
	local := make(chan string, 1)
	mux := http.NewServeMux()
	mux.HandleFunc("POST /foreign", func(writer http.ResponseWriter, request *http.Request) {
		http.Redirect(writer, request, foreignServer.URL+"/sink", http.StatusTemporaryRedirect)
	})
	mux.HandleFunc("POST /elsewhere", func(writer http.ResponseWriter, request *http.Request) {
		http.Redirect(writer, request, "/sink", http.StatusTemporaryRedirect)
	})
	mux.HandleFunc("POST /sink", func(writer http.ResponseWriter, request *http.Request) {
		local <- request.Header.Get("Authorization")
		writer.WriteHeader(http.StatusBadRequest)
	})
	origin := httptest.NewServer(mux)
	defer origin.Close()
	t.Setenv(name, "secret")
	for _, path := range []string{"/foreign", "/elsewhere"} {
		options := mcp.AuthOptions{Name: "web", URL: origin.URL + path, BearerTokenEnv: name}
		if _, err := connectWith(t, origin.URL+path, options); err == nil {
			t.Fatalf("connect through %s succeeded against a 400 sink", path)
		}
	}
	if got := <-foreign; got != "" {
		t.Fatalf("authorization reached another origin: %q", got)
	}
	if got := <-local; got != "Bearer secret" {
		t.Fatalf("same-origin redirect authorization = %q, want the bearer token", got)
	}
}

type redirectHeaders struct {
	authorization string
	sessionID     string
	protocol      string
}

func captureRedirectHeaders(writer http.ResponseWriter, request *http.Request, sink chan<- redirectHeaders) {
	sink <- redirectHeaders{
		authorization: request.Header.Get("Authorization"),
		sessionID:     request.Header.Get("Mcp-Session-Id"),
		protocol:      request.Header.Get("MCP-Protocol-Version"),
	}
	writer.WriteHeader(http.StatusAccepted)
}

func TestSessionHeadersStayOnTheirOrigin(t *testing.T) {
	foreignSink := make(chan redirectHeaders, 4)
	foreignServer := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		captureRedirectHeaders(writer, request, foreignSink)
	}))
	defer foreignServer.Close()
	localSink := make(chan redirectHeaders, 4)
	mux := http.NewServeMux()
	answerInitialize := func(writer http.ResponseWriter, request *http.Request, message testRequest) {
		writer.Header().Set("Mcp-Session-Id", "session-123")
		writeHTTPJSON(t, writer, message.ID, initResult)
	}
	handlePost := func(redirect func(http.ResponseWriter, *http.Request)) http.HandlerFunc {
		return func(writer http.ResponseWriter, request *http.Request) {
			message, err := decodeRequest(request.Body)
			if err != nil {
				writer.WriteHeader(http.StatusBadRequest)
				return
			}
			if message.Method == "initialize" {
				answerInitialize(writer, request, message)
				return
			}
			redirect(writer, request)
		}
	}
	mux.HandleFunc("POST /session-foreign", handlePost(func(writer http.ResponseWriter, request *http.Request) {
		http.Redirect(writer, request, foreignServer.URL+"/sink", http.StatusTemporaryRedirect)
	}))
	mux.HandleFunc("POST /session-local", handlePost(func(writer http.ResponseWriter, request *http.Request) {
		http.Redirect(writer, request, "/session-local-sink", http.StatusTemporaryRedirect)
	}))
	mux.HandleFunc("POST /session-local-sink", func(writer http.ResponseWriter, request *http.Request) {
		captureRedirectHeaders(writer, request, localSink)
	})
	mux.HandleFunc("DELETE /session-foreign", func(writer http.ResponseWriter, request *http.Request) {
		writer.WriteHeader(http.StatusOK)
	})
	mux.HandleFunc("DELETE /session-local", func(writer http.ResponseWriter, request *http.Request) {
		writer.WriteHeader(http.StatusOK)
	})
	origin := httptest.NewServer(mux)
	defer origin.Close()

	t.Run("cross-origin redirect drops session headers", func(t *testing.T) {
		const name = "MCP_SESSION_FOREIGN_REDIRECT_TEST"
		t.Setenv(name, "sentinel-foreign")
		options := mcp.AuthOptions{Name: "web", URL: origin.URL + "/session-foreign", BearerTokenEnv: name}
		client, err := connectWith(t, origin.URL+"/session-foreign", options)
		if err != nil {
			t.Fatalf("connect through a cross-origin redirect: %v", err)
		}
		defer func() { _ = client.Close() }()
		got := <-foreignSink
		if got != (redirectHeaders{}) {
			t.Fatalf("cross-origin redirect carried headers %+v, want none", got)
		}
	})

	t.Run("same-origin redirect keeps session headers", func(t *testing.T) {
		const name = "MCP_SESSION_LOCAL_REDIRECT_TEST"
		t.Setenv(name, "sentinel-local")
		options := mcp.AuthOptions{Name: "web", URL: origin.URL + "/session-local", BearerTokenEnv: name}
		client, err := connectWith(t, origin.URL+"/session-local", options)
		if err != nil {
			t.Fatalf("connect through a same-origin redirect: %v", err)
		}
		defer func() { _ = client.Close() }()
		got := <-localSink
		want := redirectHeaders{
			authorization: "Bearer sentinel-local",
			sessionID:     "session-123",
			protocol:      "2025-11-25",
		}
		if got != want {
			t.Fatalf("same-origin redirect headers = %+v, want %+v", got, want)
		}
	})
}

func TestTokenSourceReloadsRegistration(t *testing.T) {
	server, _ := newAuthServer(t, acceptOnly("stored-access"))
	tokenServer, endpoint := newTokenEndpoint(t, http.StatusOK)
	env := newOAuthEnv(t, server.URL+"/mcp")
	env.setCredential(t, env.resource, "stored-access")
	env.writeRegistration(t, env.resource, tokenServer.URL, "client-1")
	source, err := mcp.NewTokenSource(env.options)
	if err != nil {
		t.Fatal(err)
	}
	if token, tokenErr := source.Token(t.Context()); tokenErr != nil || token != "stored-access" {
		t.Fatalf("bound token = %q, %v", token, tokenErr)
	}
	env.writeRegistration(t, "https://other.example/mcp", tokenServer.URL, "client-1")
	if _, tokenErr := source.Token(t.Context()); tokenErr == nil {
		t.Fatal("token load accepted changed resource")
	}
	if _, refreshErr := source.Refresh(t.Context()); refreshErr == nil {
		t.Fatal("refresh accepted changed resource")
	}
	if got := endpoint.grants.Load(); got != 0 {
		t.Fatalf("mismatched registration sent %d refresh grants", got)
	}
	env.writeRegistration(t, env.resource, tokenServer.URL, "client-1")
	if token, refreshErr := source.Refresh(t.Context()); refreshErr != nil || token != "fresh-access" {
		t.Fatalf("restored registration refresh = %q, %v", token, refreshErr)
	}
}

func TestWriteRegistration(t *testing.T) {
	home := t.TempDir()
	first := mcp.Registration{Resource: "https://a.example/mcp", Issuer: "https://a.example", AuthorizationEndpoint: "https://a.example/authorize", TokenEndpoint: "https://a.example/token", ClientID: "client-a", Scopes: []string{"read"}}  //nolint:gosec // test fixture identifier, not a credential
	second := mcp.Registration{Resource: "https://b.example/mcp", Issuer: "https://b.example", AuthorizationEndpoint: "https://b.example/authorize", TokenEndpoint: "https://b.example/token", ClientID: "client-b", Scopes: []string{"read"}} //nolint:gosec // test fixture identifier, not a credential
	if err := mcp.WriteRegistration(home, "a", first); err != nil {
		t.Fatalf("write first: %v", err)
	}
	if err := mcp.WriteRegistration(home, "b", second); err != nil {
		t.Fatalf("write second: %v", err)
	}
	info, err := os.Stat(filepath.Join(home, mcp.RegistrationFile))
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("mode = %o", info.Mode().Perm())
	}
	if got, readErr := mcp.ReadRegistration(home, "a"); readErr != nil || !reflect.DeepEqual(got, first) {
		t.Fatalf("read a = %+v, %v", got, readErr)
	}
	if got, readErr := mcp.ReadRegistration(home, "b"); readErr != nil || !reflect.DeepEqual(got, second) {
		t.Fatalf("read b = %+v, %v", got, readErr)
	}
}

func TestWriteRegistrationConcurrent(t *testing.T) {
	home := t.TempDir()
	registration := func(host string) mcp.Registration {
		return mcp.Registration{Resource: "https://" + host + "/mcp", Issuer: "https://" + host, AuthorizationEndpoint: "https://" + host + "/authorize", TokenEndpoint: "https://" + host + "/token", ClientID: "client-" + host}
	}
	done := make(chan error, 2)
	for _, name := range []string{"a", "b"} {
		go func() { done <- mcp.WriteRegistration(home, name, registration(name+".example")) }()
	}
	for range 2 {
		if err := <-done; err != nil {
			t.Fatalf("write: %v", err)
		}
	}
	for _, name := range []string{"a", "b"} {
		if got, err := mcp.ReadRegistration(home, name); err != nil || got.ClientID != "client-"+name+".example" {
			t.Fatalf("read %s = %+v, %v", name, got, err)
		}
	}
}
