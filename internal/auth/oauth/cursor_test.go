package oauth_test

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/auth/oauth"
)

func TestCursorPollDeepLinkAndPoll(t *testing.T) {
	var polls atomic.Int32
	var link cursorLinkState
	expires := time.Now().Add(time.Hour).Unix()
	access := "x." + base64.RawURLEncoding.EncodeToString([]byte(fmt.Sprintf(`{"exp":%d}`, expires))) + ".x"
	server := scopedServer(t, link.pollHandler(t, &polls, access))
	defer server.Close()
	provider := oauth.Provider{ID: "cursor", AuthorizeURL: "https://example.test/login", PollURL: server.URL}
	token, err := oauth.CursorPoll(context.Background(), provider, link.open, nil)
	if err != nil {
		t.Fatalf("Cursor poll: %v", err)
	}
	if polls.Load() != 2 {
		t.Fatalf("polls = %d", polls.Load())
	}
	if token.Access != access || token.Refresh != "cursor-refresh" {
		t.Fatalf("Cursor token = %+v", token)
	}
	if token.Expires.Unix() != expires {
		t.Fatalf("Cursor expiry = %v, want %d", token.Expires, expires)
	}
}

type cursorLinkState struct {
	mu         sync.RWMutex
	identifier string
	challenge  string
}

func (link *cursorLinkState) open(rawURL string) error {
	deepLink, err := url.Parse(rawURL)
	if err != nil {
		return err
	}
	query := deepLink.Query()
	identifier := query.Get("uuid")
	challenge := query.Get("challenge")
	if !validUUID(identifier) {
		return fmt.Errorf("invalid Cursor UUID %q", identifier)
	}
	if query.Get("mode") != "login" || query.Get("redirectTarget") != "cli" {
		return fmt.Errorf("deep-link query = %v", query)
	}
	link.mu.Lock()
	link.identifier = identifier
	link.challenge = challenge
	link.mu.Unlock()
	return nil
}

func (link *cursorLinkState) pollHandler(t *testing.T, polls *atomic.Int32, access string) http.HandlerFunc {
	t.Helper()
	return func(writer http.ResponseWriter, request *http.Request) {
		link.checkQuery(t, request.URL.Query())
		if polls.Add(1) == 1 {
			writer.WriteHeader(http.StatusNotFound)
			return
		}
		writer.Header().Set("Content-Type", "application/json")
		_, _ = fmt.Fprintf(writer, `{"accessToken":%q,"refreshToken":"cursor-refresh"}`, access)
	}
}

func (link *cursorLinkState) checkQuery(t *testing.T, query url.Values) {
	t.Helper()
	link.mu.RLock()
	identifier := link.identifier
	challenge := link.challenge
	link.mu.RUnlock()
	if query.Get("uuid") != identifier {
		t.Errorf("poll uuid = %q, want %q", query.Get("uuid"), identifier)
	}
	digest := sha256.Sum256([]byte(query.Get("verifier")))
	actualChallenge := base64.RawURLEncoding.EncodeToString(digest[:])
	if actualChallenge != challenge {
		t.Errorf("poll verifier does not match deep-link challenge")
	}
}

func TestCursorRefreshUsesSeparateJSONEndpoint(t *testing.T) {
	var pollEndpointCalls atomic.Int32
	pollServer := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		pollEndpointCalls.Add(1)
		writer.WriteHeader(http.StatusInternalServerError)
	}))
	defer pollServer.Close()
	refreshServer := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		body, err := io.ReadAll(request.Body)
		if err != nil {
			t.Errorf("read Cursor refresh: %v", err)
		}
		text := string(body)
		for _, fragment := range []string{`"grant_type":"refresh_token"`, `"client_id":"cursor-client"`, `"refresh_token":"stored-refresh"`} {
			if !strings.Contains(text, fragment) {
				t.Errorf("Cursor refresh body %s lacks %s", text, fragment)
			}
		}
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"access_token":"renewed-access"}`)
	}))
	defer refreshServer.Close()
	provider := oauth.Provider{ID: "cursor", TokenURL: pollServer.URL, RefreshURL: refreshServer.URL, ClientID: "cursor-client"}
	token, err := oauth.Refresh(context.Background(), provider, "stored-refresh")
	if err != nil {
		t.Fatalf("Cursor refresh: %v", err)
	}
	if token.Access != "renewed-access" || token.Refresh != "stored-refresh" {
		t.Fatalf("Cursor refresh result = %+v", token)
	}
	if pollEndpointCalls.Load() != 0 {
		t.Fatalf("refresh called poll endpoint")
	}
}

func TestCursorRefreshRejectsEndedSession(t *testing.T) {
	server := scopedServer(t, http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.Header().Set("Content-Type", "application/json")
		_, _ = io.WriteString(writer, `{"access_token":"","shouldLogout":true}`)
	}))
	defer server.Close()
	_, err := oauth.Refresh(context.Background(), oauth.Provider{ID: "cursor", RefreshURL: server.URL}, "old")
	if err == nil || err.Error() != "invalid_grant: Cursor ended this session; run /login cursor again" {
		t.Fatalf("ended-session error = %v", err)
	}
}

func validUUID(value string) bool {
	if len(value) != 36 || value[8] != '-' || value[13] != '-' || value[18] != '-' || value[23] != '-' {
		return false
	}
	compact := strings.ReplaceAll(value, "-", "")
	decoded, err := hex.DecodeString(compact)
	return err == nil && len(decoded) == 16 && decoded[6]>>4 == 4 && decoded[8]&0xc0 == 0x80
}
