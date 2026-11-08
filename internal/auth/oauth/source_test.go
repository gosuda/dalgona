package oauth_test

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/auth/oauth"
)

func refreshServer(t *testing.T, calls *atomic.Int32, status *atomic.Int32) *httptest.Server {
	t.Helper()
	server := scopedServer(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		if err := r.ParseForm(); err != nil || r.PostForm.Get("refresh_token") != "old-refresh" || r.PostForm.Get("grant_type") != "refresh_token" {
			http.Error(w, "bad refresh request", http.StatusBadRequest)
			return
		}
		time.Sleep(20 * time.Millisecond)
		w.WriteHeader(int(status.Load()))
		_, _ = w.Write([]byte(`{"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}`))
	}))
	t.Cleanup(server.Close)
	return server
}

func statusOf(code int32) *atomic.Int32 {
	status := new(atomic.Int32)
	status.Store(code)
	return status
}

func expiredToken() oauth.Token {
	return oauth.Token{Access: "old-access", Refresh: "old-refresh", Expires: time.Now().Add(30 * time.Second), Account: "acct-1"}
}

func TestSourceRefreshesOnceUnderConcurrentCallers(t *testing.T) {
	var calls atomic.Int32
	server := refreshServer(t, &calls, statusOf(http.StatusOK))
	var saved []oauth.Token
	var saveMu sync.Mutex
	source := oauth.NewSource(oauth.Provider{ID: "test", TokenURL: server.URL},
		func(context.Context) (oauth.Token, error) { return expiredToken(), nil },
		func(_ context.Context, token oauth.Token) error {
			saveMu.Lock()
			defer saveMu.Unlock()
			saved = append(saved, token)
			return nil
		})
	access := concurrentAccess(t, source, 8)
	if got := calls.Load(); got != 1 {
		t.Fatalf("refresh calls = %d, want 1", got)
	}
	for index, value := range access {
		if value != "new-access" {
			t.Fatalf("caller %d access = %q", index, value)
		}
	}
	checkSaved(t, saved)
}

func concurrentAccess(t *testing.T, source *oauth.Source, callers int) []string {
	t.Helper()
	var wg sync.WaitGroup
	access := make([]string, callers)
	for index := range access {
		wg.Go(func() {
			token, err := source.Token(t.Context())
			if err != nil {
				t.Errorf("token: %v", err)
			}
			access[index] = token.Access
		})
	}
	wg.Wait()
	return access
}

func checkSaved(t *testing.T, saved []oauth.Token) {
	t.Helper()
	if len(saved) != 1 || saved[0].Access != "new-access" || saved[0].Refresh != "new-refresh" || saved[0].Account != "acct-1" {
		t.Fatalf("saved = %+v", saved)
	}
	if time.Until(saved[0].Expires) < 59*time.Minute {
		t.Fatalf("saved expiry %v is not about one hour ahead", saved[0].Expires)
	}
}

func TestSourceKeepsValidTokenWithoutRefresh(t *testing.T) {
	var calls atomic.Int32
	server := refreshServer(t, &calls, statusOf(http.StatusOK))
	valid := oauth.Token{Access: "valid", Refresh: "old-refresh", Expires: time.Now().Add(time.Hour)}
	source := oauth.NewSource(oauth.Provider{ID: "test", TokenURL: server.URL},
		func(context.Context) (oauth.Token, error) { return valid, nil }, nil)
	access, err := source.Access(t.Context())
	if err != nil || access != "valid" || calls.Load() != 0 {
		t.Fatalf("access = %q, err = %v, refresh calls = %d", access, err, calls.Load())
	}
}

func TestSourceRefreshErrorKeepsOldToken(t *testing.T) {
	var calls atomic.Int32
	status := statusOf(http.StatusServiceUnavailable)
	server := refreshServer(t, &calls, status)
	var loads, saves atomic.Int32
	source := oauth.NewSource(oauth.Provider{ID: "test", TokenURL: server.URL},
		func(context.Context) (oauth.Token, error) { loads.Add(1); return expiredToken(), nil },
		func(context.Context, oauth.Token) error { saves.Add(1); return nil })
	if _, err := source.Token(t.Context()); err == nil {
		t.Fatal("refresh failure returned no error")
	}
	if saves.Load() != 0 {
		t.Fatal("save called after failed refresh")
	}
	status.Store(http.StatusOK)
	token, err := source.Token(t.Context())
	if err != nil || token.Access != "new-access" {
		t.Fatalf("token after recovery = %+v, err = %v; the old refresh token was not kept", token, err)
	}
	if calls.Load() != 2 || loads.Load() != 1 || saves.Load() != 1 {
		t.Fatalf("refresh calls = %d, loads = %d, saves = %d", calls.Load(), loads.Load(), saves.Load())
	}
}

func TestSourceSaveFailureKeepsRotatedTokenAndRetriesSave(t *testing.T) {
	var calls, saves atomic.Int32
	server := refreshServer(t, &calls, statusOf(http.StatusOK))
	source := oauth.NewSource(oauth.Provider{ID: "test", TokenURL: server.URL},
		func(context.Context) (oauth.Token, error) { return expiredToken(), nil },
		func(_ context.Context, token oauth.Token) error {
			if saves.Add(1) == 1 {
				return errors.New("disk full")
			}
			if token.Refresh != "new-refresh" {
				t.Errorf("saved refresh = %q", token.Refresh)
			}
			return nil
		})
	if _, err := source.Token(t.Context()); err == nil {
		t.Fatal("save failure returned no error")
	}
	token, err := source.Token(t.Context())
	if err != nil || token.Access != "new-access" || calls.Load() != 1 || saves.Load() != 2 {
		t.Fatalf("token = %+v, err = %v, refresh calls = %d, saves = %d", token, err, calls.Load(), saves.Load())
	}
}
