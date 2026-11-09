package auth_test

import (
	"context"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
)

type casFixture struct {
	home   string
	store  *auth.Store
	first  auth.Credential
	second auth.Credential
}

func TestStore_SetCredentialIfUnchanged(t *testing.T) {
	t.Setenv("CURSOR_API_KEY", "")
	t.Setenv("OPENAI_API_KEY", "")
	t.Setenv("GOOGLE_API_KEY", "")
	home := t.TempDir()
	fx := casFixture{
		home:   home,
		store:  loadCredentialStore(t, home),
		first:  auth.Credential{Type: "oauth", Access: "a1", Refresh: "r1"},
		second: auth.Credential{Type: "oauth", Access: "a2", Refresh: "r2"},
	}
	seedCredential(t, fx.store, "cursor", fx.first)
	assertCredentialMatch(t, fx)
	assertStaleCredentialKept(t, fx)
	assertAbsentCredentialCreated(t, fx.store)
	assertAbsentCredentialMismatch(t, fx.store, fx.first)
}

func loadCredentialStore(t *testing.T, home string) *auth.Store {
	t.Helper()
	store, loadErr := auth.Load(home)
	if loadErr != nil {
		t.Fatalf("load: %v", loadErr)
	}
	return store
}

func seedCredential(t *testing.T, store *auth.Store, provider string, cred auth.Credential) {
	t.Helper()
	if seedErr := store.SetCredential(provider, cred); seedErr != nil {
		t.Fatalf("seed: %v", seedErr)
	}
}

func assertCredentialMatch(t *testing.T, fx casFixture) {
	t.Helper()
	written, casErr := fx.store.SetCredentialIfUnchanged("cursor", fx.first, fx.second)
	if casErr != nil || !written {
		t.Fatalf("CAS on match: written = %v, err = %v", written, casErr)
	}
	reloaded := loadCredentialStore(t, fx.home)
	got, tokenErr := reloaded.Token(context.Background(), "cursor")
	if tokenErr != nil || got != fx.second {
		t.Fatalf("stored = %+v (err %v), want %+v", got, tokenErr, fx.second)
	}
}

func assertStaleCredentialKept(t *testing.T, fx casFixture) {
	t.Helper()
	third := auth.Credential{Type: "oauth", Access: "a3", Refresh: "r3"}
	written, casErr := fx.store.SetCredentialIfUnchanged("cursor", fx.first, third)
	if casErr != nil {
		t.Fatalf("stale CAS: %v", casErr)
	}
	if written {
		t.Fatal("stale CAS overwrote a newer credential")
	}
	reloaded := loadCredentialStore(t, fx.home)
	got, tokenErr := reloaded.Token(context.Background(), "cursor")
	if tokenErr != nil || got != fx.second {
		t.Fatalf("after stale CAS stored = %+v (err %v), want %+v", got, tokenErr, fx.second)
	}
}

func assertAbsentCredentialCreated(t *testing.T, store *auth.Store) {
	t.Helper()
	written, casErr := store.SetCredentialIfUnchanged("openai", auth.Credential{}, auth.Credential{Type: "api_key", Key: "k"})
	if casErr != nil || !written {
		t.Fatalf("CAS on absent provider: written = %v, err = %v", written, casErr)
	}
}

func assertAbsentCredentialMismatch(t *testing.T, store *auth.Store, previous auth.Credential) {
	t.Helper()
	written, casErr := store.SetCredentialIfUnchanged("google", previous, auth.Credential{Type: "api_key", Key: "g"})
	if casErr != nil {
		t.Fatalf("CAS absent mismatch: %v", casErr)
	}
	if written {
		t.Fatal("CAS absent mismatch wrote a credential")
	}
}
