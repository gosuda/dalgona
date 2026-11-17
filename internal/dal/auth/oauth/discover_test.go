package oauth_test

import (
	"context"
	"encoding/json/v2"
	"io"
	"net/http"
	"net/http/httptest"
	"net/url"
	"slices"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
)

type metadataFixture struct {
	mu    sync.Mutex
	paths []string
	serve func(path, base string) (int, string)
}

func newMetadataServer(t *testing.T, serve func(path, base string) (int, string)) (*httptest.Server, *metadataFixture) {
	t.Helper()
	fixture := &metadataFixture{serve: serve}
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		fixture.mu.Lock()
		fixture.paths = append(fixture.paths, request.URL.Path)
		fixture.mu.Unlock()
		status, body := fixture.serve(request.URL.Path, "http://"+request.Host)
		writer.Header().Set("Content-Type", "application/json")
		writer.WriteHeader(status)
		_, _ = io.WriteString(writer, body)
	}))
	t.Cleanup(server.Close)
	return server, fixture
}

func (f *metadataFixture) requested() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	return slices.Clone(f.paths)
}

func resourceDocument(resource, servers string) string {
	return `{"resource":"` + resource + `","authorization_servers":` + servers + `,"scopes_supported":["read","write"]}`
}

func serverDocument(issuer, base, methods string) string {
	return `{"issuer":"` + issuer + `","authorization_endpoint":"` + base + `/authorize","token_endpoint":"` + base + `/token","registration_endpoint":"` + base + `/register","code_challenge_methods_supported":` + methods + `}`
}

func discoveryRoutes(path, base string) (int, string) {
	switch path {
	case "/.well-known/oauth-protected-resource":
		return http.StatusOK, resourceDocument(base+"/mcp", `["`+base+`"]`)
	case "/.well-known/openid-configuration":
		return http.StatusOK, serverDocument(base, base, `["plain","S256"]`)
	default:
		return http.StatusNotFound, `{}`
	}
}

func TestDiscoveryOrder(t *testing.T) {
	t.Run("path then root then oauth then openid", func(t *testing.T) { checkDiscoveryOrder(t) })
	t.Run("header names the metadata url", func(t *testing.T) { checkHeaderMetadataURL(t) })
	t.Run("resource mismatch", func(t *testing.T) { checkResourceMismatch(t) })
	t.Run("server without S256", func(t *testing.T) { checkServerWithoutS256(t) })
	t.Run("issuer mismatch", func(t *testing.T) { checkIssuerMismatch(t) })
	t.Run("insecure authorization server", func(t *testing.T) { checkInsecureAuthorizationServer(t) })
}

func checkDiscoveryOrder(t *testing.T) {
	t.Helper()
	server, fixture := newMetadataServer(t, discoveryRoutes)
	resource, err := oauth.DiscoverResource(t.Context(), server.Client(), server.URL+"/mcp", `Bearer realm="mcp"`)
	if err != nil {
		t.Fatalf("DiscoverResource: %v", err)
	}
	if resource.URL != server.URL+"/mcp" || resource.AuthorizationServer != server.URL || !slices.Equal(resource.Scopes, []string{"read", "write"}) {
		t.Fatalf("resource = %+v", resource)
	}
	meta, err := oauth.DiscoverServer(t.Context(), server.Client(), resource.AuthorizationServer)
	if err != nil {
		t.Fatalf("DiscoverServer: %v", err)
	}
	want := oauth.ServerMetadata{
		Issuer: server.URL, AuthorizationEndpoint: server.URL + "/authorize",
		TokenEndpoint: server.URL + "/token", RegistrationEndpoint: server.URL + "/register",
	}
	if meta != want {
		t.Fatalf("metadata = %+v, want %+v", meta, want)
	}
	wantPaths := []string{
		"/.well-known/oauth-protected-resource/mcp", "/.well-known/oauth-protected-resource",
		"/.well-known/oauth-authorization-server", "/.well-known/openid-configuration",
	}
	if got := fixture.requested(); !slices.Equal(got, wantPaths) {
		t.Fatalf("request order = %v, want %v", got, wantPaths)
	}
}

func checkHeaderMetadataURL(t *testing.T) {
	t.Helper()
	server, fixture := newMetadataServer(t, func(path, base string) (int, string) {
		if path == "/meta/doc" {
			return http.StatusOK, resourceDocument(base+"/mcp", `["`+base+`"]`)
		}
		return http.StatusNotFound, `{}`
	})
	header := `Bearer error="invalid_token", resource_metadata="` + server.URL + `/meta/doc"`
	if _, err := oauth.DiscoverResource(t.Context(), server.Client(), server.URL+"/mcp", header); err != nil {
		t.Fatalf("DiscoverResource: %v", err)
	}
	if got := fixture.requested(); !slices.Equal(got, []string{"/meta/doc"}) {
		t.Fatalf("requests = %v, want only the header URL", got)
	}
}

func checkResourceMismatch(t *testing.T) {
	t.Helper()
	server, _ := newMetadataServer(t, func(path, base string) (int, string) {
		if path == "/.well-known/oauth-protected-resource" {
			return http.StatusOK, resourceDocument(base+"/other", `["`+base+`"]`)
		}
		return http.StatusNotFound, `{}`
	})
	_, err := oauth.DiscoverResource(t.Context(), server.Client(), server.URL+"/mcp", "")
	want := "oauth: protected resource metadata names " + server.URL + "/other, want " + server.URL + "/mcp"
	if err == nil || err.Error() != want {
		t.Fatalf("error = %v, want %q", err, want)
	}
}

func checkServerWithoutS256(t *testing.T) {
	t.Helper()
	server, _ := newMetadataServer(t, func(path, base string) (int, string) {
		if path == "/.well-known/oauth-authorization-server" {
			return http.StatusOK, serverDocument(base, base, `["plain"]`)
		}
		return http.StatusNotFound, `{}`
	})
	_, err := oauth.DiscoverServer(t.Context(), server.Client(), server.URL)
	want := "oauth: server " + server.URL + " does not support PKCE S256"
	if err == nil || err.Error() != want {
		t.Fatalf("error = %v, want %q", err, want)
	}
}

func checkIssuerMismatch(t *testing.T) {
	t.Helper()
	server, _ := newMetadataServer(t, func(path, base string) (int, string) {
		if path == "/.well-known/oauth-authorization-server" {
			return http.StatusOK, serverDocument("https://other.example.test", base, `["S256"]`)
		}
		return http.StatusNotFound, `{}`
	})
	_, err := oauth.DiscoverServer(t.Context(), server.Client(), server.URL)
	want := "oauth: server metadata names issuer https://other.example.test, want " + server.URL
	if err == nil || err.Error() != want {
		t.Fatalf("error = %v, want %q", err, want)
	}
}

func checkInsecureAuthorizationServer(t *testing.T) {
	t.Helper()
	server, fixture := newMetadataServer(t, func(path, base string) (int, string) {
		if path == "/.well-known/oauth-protected-resource" {
			return http.StatusOK, resourceDocument(base+"/mcp", `["http://auth.example.test"]`)
		}
		return http.StatusNotFound, `{}`
	})
	_, err := oauth.DiscoverResource(t.Context(), server.Client(), server.URL+"/mcp", "")
	want := "oauth: authorization server http://auth.example.test is not https"
	if err == nil || err.Error() != want {
		t.Fatalf("DiscoverResource error = %v, want %q", err, want)
	}
	before := len(fixture.requested())
	if _, err := oauth.DiscoverServer(t.Context(), server.Client(), "http://auth.example.test"); err == nil || err.Error() != want {
		t.Fatalf("DiscoverServer error = %v, want %q", err, want)
	}
	if after := len(fixture.requested()); after != before {
		t.Fatalf("DiscoverServer made %d requests for an insecure issuer", after-before)
	}
}

func TestRegisterPostsPublicClient(t *testing.T) {
	bodies := make(chan []byte, 1)
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		body, err := io.ReadAll(request.Body)
		if err != nil || request.Method != http.MethodPost || request.Header.Get("Content-Type") != "application/json" {
			t.Errorf("registration request = %s %q, read error %v", request.Method, request.Header.Get("Content-Type"), err)
		}
		bodies <- body
		writer.Header().Set("Content-Type", "application/json")
		writer.WriteHeader(http.StatusCreated)
		_, _ = io.WriteString(writer, `{"client_id":"client-abc"}`)
	}))
	defer server.Close()
	redirect := "http://127.0.0.1:54871/callback"
	clientID, err := oauth.Register(t.Context(), server.Client(), oauth.ServerMetadata{RegistrationEndpoint: server.URL + "/register"}, redirect, nil)
	if err != nil || clientID != "client-abc" {
		t.Fatalf("Register = %q, %v", clientID, err)
	}
	checkRegistrationRequest(t, <-bodies, redirect, "")
}

func TestRegisterScopes(t *testing.T) {
	bodies := make(chan []byte, 1)
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		body, err := io.ReadAll(request.Body)
		if err != nil {
			t.Errorf("read body: %v", err)
		}
		bodies <- body
		writer.WriteHeader(http.StatusCreated)
		_, _ = io.WriteString(writer, `{"client_id":"client-abc"}`)
	}))
	defer server.Close()
	redirect := "http://127.0.0.1:54871/callback"
	if _, err := oauth.Register(t.Context(), server.Client(), oauth.ServerMetadata{RegistrationEndpoint: server.URL}, redirect, []string{"read", "write"}); err != nil {
		t.Fatalf("Register: %v", err)
	}
	checkRegistrationRequest(t, <-bodies, redirect, "read write")
}

func checkRegistrationRequest(t *testing.T, body []byte, redirect, scope string) {
	t.Helper()
	var got struct {
		ClientName    string   `json:"client_name"`
		RedirectURIs  []string `json:"redirect_uris"`
		GrantTypes    []string `json:"grant_types"`
		ResponseTypes []string `json:"response_types"`
		AuthMethod    string   `json:"token_endpoint_auth_method"`
		Scope         *string  `json:"scope"`
	}
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatalf("decode registration request: %v", err)
	}
	if got.ClientName != "dalgona" || got.AuthMethod != "none" ||
		!slices.Equal(got.RedirectURIs, []string{redirect}) ||
		!slices.Equal(got.GrantTypes, []string{"authorization_code", "refresh_token"}) ||
		!slices.Equal(got.ResponseTypes, []string{"code"}) {
		t.Fatalf("registration request = %+v", got)
	}
	checkRegistrationScope(t, got.Scope, scope)
}

func checkRegistrationScope(t *testing.T, got *string, want string) {
	t.Helper()
	if want == "" {
		if got != nil {
			t.Fatalf("registration request scope = %q, want omitted", *got)
		}
		return
	}
	if got == nil || *got != want {
		t.Fatalf("registration request scope = %v, want %q", got, want)
	}
}

func TestRegisterRefusals(t *testing.T) {
	_, err := oauth.Register(t.Context(), nil, oauth.ServerMetadata{}, "http://127.0.0.1:54871/callback", nil)
	if want := "oauth: server has no registration endpoint; set oauth_client_id"; err == nil || err.Error() != want {
		t.Fatalf("no endpoint error = %v, want %q", err, want)
	}
	for _, tc := range []struct {
		name   string
		status int
		body   string
		want   string
	}{
		{"http error", http.StatusBadRequest, `{"error":"invalid_redirect_uri"}`, "oauth: registration endpoint returned HTTP 400"},
		{"no client id", http.StatusCreated, `{}`, "oauth: registration response has no client_id"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) {
				writer.WriteHeader(tc.status)
				_, _ = io.WriteString(writer, tc.body)
			}))
			defer server.Close()
			_, err := oauth.Register(t.Context(), server.Client(), oauth.ServerMetadata{RegistrationEndpoint: server.URL}, "http://127.0.0.1:54871/callback", nil)
			if err == nil || err.Error() != tc.want {
				t.Fatalf("error = %v, want %q", err, tc.want)
			}
		})
	}
}

func TestProviderResourceReachesEveryRequest(t *testing.T) {
	for _, tc := range []struct{ name, resource string }{
		{"bound", "https://mcp.example.test/mcp"},
		{"unbound", ""},
	} {
		t.Run(tc.name, func(t *testing.T) { checkResourceParameter(t, tc.resource) })
	}
}

func checkResourceParameter(t *testing.T, resource string) {
	t.Helper()
	forms := make(chan url.Values, 2)
	tokenServer := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if err := request.ParseForm(); err != nil {
			t.Errorf("parse token form: %v", err)
			writer.WriteHeader(http.StatusBadRequest)
			return
		}
		forms <- request.PostForm
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"access_token":"access","refresh_token":"refresh","expires_in":3600}`)
	}))
	defer tokenServer.Close()
	provider := testProvider(tokenServer.URL, freePort(t), 0)
	provider.Resource = resource
	authorizations := make(chan url.Values, 1)
	callback := make(chan callbackPage, 1)
	open := func(rawURL string) error {
		authURL, err := url.Parse(rawURL)
		if err != nil {
			return err
		}
		query := authURL.Query()
		authorizations <- query
		requestPage(query.Get("redirect_uri")+"?code=browser-code&state="+url.QueryEscape(query.Get("state")), callback)
		return nil
	}
	if _, err := oauth.PKCE(t.Context(), provider, open, nil); err != nil {
		t.Fatalf("PKCE: %v", err)
	}
	if _, err := oauth.Refresh(t.Context(), provider, "refresh"); err != nil {
		t.Fatalf("Refresh: %v", err)
	}
	if result := <-callback; result.err != nil {
		t.Fatalf("callback: %v", result.err)
	}
	exchangeForm := <-forms
	for name, values := range map[string]url.Values{"authorization": <-authorizations, "exchange": exchangeForm, "refresh": <-forms} {
		if got := values.Get("resource"); got != resource {
			t.Errorf("%s resource = %q, want %q", name, got, resource)
		}
	}
	if (exchangeForm.Get("state") != "") != (resource == "") {
		t.Errorf("exchange state = %q with resource %q", exchangeForm.Get("state"), resource)
	}
}

func TestSourceRefreshForcesGrantOnValidToken(t *testing.T) {
	var calls atomic.Int32
	server := refreshServer(t, &calls, statusOf(http.StatusOK))
	valid := oauth.Token{Access: "valid", Refresh: "old-refresh", Expires: time.Now().Add(time.Hour), Account: "acct"}
	var saved []oauth.Token
	source := oauth.NewSource(oauth.Provider{ID: "test", TokenURL: server.URL},
		func(context.Context) (oauth.Token, error) { return valid, nil },
		func(_ context.Context, token oauth.Token) error { saved = append(saved, token); return nil })
	token, err := source.Refresh(t.Context())
	if err != nil || token.Access != "new-access" || token.Account != "acct" || calls.Load() != 1 || len(saved) != 1 {
		t.Fatalf("Refresh = %+v, %v, grants = %d, saves = %d", token, err, calls.Load(), len(saved))
	}
	access, err := source.Access(t.Context())
	if err != nil || access != "new-access" || calls.Load() != 1 {
		t.Fatalf("Access after Refresh = %q, %v, grants = %d, want the cached token and 1 grant", access, err, calls.Load())
	}
}

func TestSourceRefreshWithoutTokenFails(t *testing.T) {
	var calls atomic.Int32
	server := refreshServer(t, &calls, statusOf(http.StatusOK))
	bare := oauth.NewSource(oauth.Provider{ID: "test", TokenURL: server.URL},
		func(context.Context) (oauth.Token, error) { return oauth.Token{Access: "valid"}, nil }, nil)
	if _, err := bare.Refresh(t.Context()); err == nil || calls.Load() != 0 {
		t.Fatalf("Refresh without a refresh token = %v, grants = %d, want an error and no grant", err, calls.Load())
	}
}
