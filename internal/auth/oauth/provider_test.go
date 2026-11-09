package oauth_test

import (
	"bytes"
	"context"
	json "encoding/json/jsontext"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/auth/oauth"
)

func TestClaudeBareCodeExchangeIncludesVerifierState(t *testing.T) {
	var exchangedState atomic.Value
	server := scopedServer(t, claudeExchangeHandler(t, &exchangedState))
	defer server.Close()
	provider := testProvider(server.URL, freePort(t), 0)
	provider.ID = "anthropic"
	provider.RedirectHost = "localhost"
	provider.BindHost = "127.0.0.1"
	var authorizationState string
	open := func(rawURL string) error {
		authURL, err := url.Parse(rawURL)
		if err != nil {
			return err
		}
		authorizationState = authURL.Query().Get("state")
		return nil
	}
	paste := func(context.Context) (string, error) { return "claude-code", nil }
	token, err := oauth.PKCE(context.Background(), provider, open, paste)
	if err != nil {
		t.Fatalf("Claude bare-code PKCE: %v", err)
	}
	if token.Access != "claude-access" {
		t.Fatalf("Claude access = %q", token.Access)
	}
	if authorizationState == "" || authorizationState != exchangedState.Load() {
		t.Fatalf("authorization state %q, exchanged state %q", authorizationState, exchangedState.Load())
	}
}

func claudeExchangeHandler(t *testing.T, exchangedState *atomic.Value) http.HandlerFunc {
	t.Helper()
	return func(writer http.ResponseWriter, request *http.Request) {
		fields := requestStringObject(t, request)
		if fields["grant_type"] != "authorization_code" || fields["code"] != "claude-code" {
			t.Errorf("Claude exchange = %v", fields)
		}
		if fields["state"] == "" || fields["state"] != fields["code_verifier"] {
			t.Errorf("Claude state %q does not use verifier", fields["state"])
		}
		if !strings.HasPrefix(fields["redirect_uri"], "http://localhost:") {
			t.Errorf("Claude redirect = %q", fields["redirect_uri"])
		}
		exchangedState.Store(fields["state"])
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"access_token":"claude-access"}`)
	}
}

func TestDevinExchangeContainsOnlyCodeAndVerifier(t *testing.T) {
	server := scopedServer(t, devinExchangeHandler(t))
	defer server.Close()
	provider := testProvider(server.URL, freePort(t), 0)
	provider.ID = "devin"
	open := func(string) error { return nil }
	paste := func(context.Context) (string, error) { return "devin-code", nil }
	token, err := oauth.PKCE(context.Background(), provider, open, paste)
	if err != nil {
		t.Fatalf("Devin PKCE: %v", err)
	}
	if token.Access != "devin-token" || token.Refresh != "devin-token" {
		t.Fatalf("Devin token = %+v", token)
	}
}

func devinExchangeHandler(t *testing.T) http.HandlerFunc {
	t.Helper()
	return func(writer http.ResponseWriter, request *http.Request) {
		fields := requestStringObject(t, request)
		if len(fields) != 2 {
			t.Errorf("Devin fields = %v, want exactly code and code_verifier", fields)
		}
		if fields["code"] != "devin-code" {
			t.Errorf("Devin code = %q", fields["code"])
		}
		if fields["code_verifier"] == "" {
			t.Error("Devin verifier is empty")
		}
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"token":"devin-token"}`)
	}
}

func requestStringObject(t *testing.T, request *http.Request) map[string]string {
	t.Helper()
	body, err := io.ReadAll(request.Body)
	if err != nil {
		t.Errorf("read exchange body: %v", err)
		return nil
	}
	return decodeStringObject(t, body)
}

func decodeStringObject(t *testing.T, body []byte) map[string]string {
	t.Helper()
	decoder := json.NewDecoder(bytes.NewReader(body))
	opening, err := decoder.ReadToken()
	if err != nil || opening.Kind() != '{' {
		t.Errorf("invalid JSON object: %v", err)
		return nil
	}
	fields := make(map[string]string)
	for decoder.PeekKind() != '}' {
		name, nameErr := decoder.ReadToken()
		if nameErr != nil {
			t.Errorf("read field name: %v", nameErr)
			return nil
		}
		key := name.String()
		value, valueErr := decoder.ReadToken()
		if valueErr != nil || value.Kind() != '"' {
			t.Errorf("field %s is not a string: %v", key, valueErr)
			return nil
		}
		fields[key] = value.String()
	}
	if _, err := decoder.ReadToken(); err != nil {
		t.Errorf("read object end: %v", err)
	}
	return fields
}

func TestAntigravityRefreshIncludesDonorClientFields(t *testing.T) {
	server := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if err := request.ParseForm(); err != nil {
			t.Errorf("parse Antigravity refresh: %v", err)
		}
		if request.Form.Get("client_id") != "public-client" || request.Form.Get("client_secret") != "public-key" {
			t.Errorf("Antigravity refresh fields = %v", request.Form)
		}
		writer.Header().Set("Content-Type", "application/json")
		_, _ = fmt.Fprint(writer, `{"access_token":"google-access"}`)
	}))
	defer server.Close()
	provider := oauth.Provider{
		ID: "antigravity", TokenURL: server.URL, ClientID: "public-client", ClientSecret: "public-key",
	}
	token, err := oauth.Refresh(context.Background(), provider, "refresh")
	if err != nil {
		t.Fatalf("Antigravity refresh: %v", err)
	}
	if token.Access != "google-access" {
		t.Fatalf("Antigravity access = %q", token.Access)
	}
}
