package oauth_test

import (
	"context"
	json "encoding/json/v2"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/auth/oauth"
)

func TestRegisterClient(t *testing.T) {
	var request map[string]any
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Errorf("read body: %v", err)
		}
		if err := json.Unmarshal(body, &request); err != nil {
			t.Errorf("decode body: %v", err)
		}
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"client_id":"dyn-1","client_secret_expires_at":0}`)
	}))
	defer server.Close()
	clientID, err := oauth.RegisterClient(context.Background(), server.Client(), server.URL+"/register", "http://127.0.0.1:54871/callback", []string{"read", "write"})
	if err != nil {
		t.Fatalf("register client: %v", err)
	}
	if clientID != "dyn-1" {
		t.Fatalf("client_id = %q", clientID)
	}
	requireRegistrationBody(t, request)
}

func requireRegistrationBody(t *testing.T, request map[string]any) {
	t.Helper()
	if request["client_name"] != "dalgona" || request["token_endpoint_auth_method"] != "none" {
		t.Fatalf("registration fields = %v", request)
	}
	if got := request["redirect_uris"]; !jsonEqualStrings(got, "http://127.0.0.1:54871/callback") {
		t.Fatalf("redirect_uris = %v", got)
	}
	if got := request["grant_types"]; !jsonEqualStrings(got, "authorization_code", "refresh_token") {
		t.Fatalf("grant_types = %v", got)
	}
	if request["scope"] != "read write" {
		t.Fatalf("scope = %v", request["scope"])
	}
}

func TestRegisterClientWithoutClientID(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) {
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"client_secret_expires_at":0}`)
	}))
	defer server.Close()
	if _, err := oauth.RegisterClient(context.Background(), server.Client(), server.URL, "http://127.0.0.1:54871/callback", nil); err == nil {
		t.Fatal("registration without client_id succeeded")
	}
}

func jsonEqualStrings(value any, want ...string) bool {
	list, ok := value.([]any)
	if !ok {
		return false
	}
	got := make([]string, len(list))
	for i, item := range list {
		s, ok := item.(string)
		if !ok {
			return false
		}
		got[i] = s
	}
	return slices.Equal(got, want)
}
