package oauth_test

import (
	"context"
	"errors"
	"net/url"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/auth/oauth"
)

func TestRequestErrorRedactsQuery(t *testing.T) {
	query := url.Values{"client_id": {"qClient"}, "verifier": {"qVerifier"}, "uuid": {"qUuid"}}
	endpoint := "http://127.0.0.1:1/oauth/token?" + query.Encode()
	provider := oauth.Provider{ID: "cursor", TokenURL: endpoint, RefreshURL: endpoint}
	_, err := oauth.Refresh(context.Background(), provider, "refresh-token")
	if err == nil {
		t.Fatal("Refresh against a closed port: expected error")
	}
	message := err.Error()
	for _, secret := range []string{"qClient", "qVerifier", "qUuid"} {
		if strings.Contains(message, secret) {
			t.Fatalf("error message %q leaks %q", message, secret)
		}
	}
	urlError, ok := errors.AsType[*url.Error](err)
	if !ok {
		t.Fatalf("error %v is not *url.Error", err)
	}
	if !strings.Contains(urlError.URL, "REDACTED") {
		t.Fatalf("redacted URL = %q, want REDACTED query", urlError.URL)
	}
}
