package app

import (
	"context"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/config"
)

func newSaveTestCredentials(t *testing.T, stored auth.Credential) (*credentials, *sourceEntry, string) {
	t.Helper()
	home := t.TempDir()
	store, err := auth.New(home)
	if err != nil {
		t.Fatalf("store: %v", err)
	}
	if setErr := store.SetCredential("cursor", stored); setErr != nil {
		t.Fatalf("seed: %v", setErr)
	}
	creds := &credentials{store: store, product: config.ProductDalgona, sources: map[string]*sourceEntry{}}
	return creds, &sourceEntry{stored: stored}, home
}

func TestCredentialsSavePersistsRefresh(t *testing.T) {
	t.Setenv("CURSOR_API_KEY", "")
	expires := time.Now().Add(time.Hour).Format(time.RFC3339)
	stored := auth.Credential{Type: "oauth", Access: "a1", Refresh: "r1", Expires: expires}
	creds, entry, home := newSaveTestCredentials(t, stored)
	fresh := auth.Credential{Type: "oauth", Access: "a2", Refresh: "r2", Expires: expires}
	if saveErr := creds.saveCredential(entry, "cursor", fresh); saveErr != nil {
		t.Fatalf("save: %v", saveErr)
	}
	if entry.stored != fresh {
		t.Fatalf("entry.stored = %+v, want %+v", entry.stored, fresh)
	}
	reloaded, err := auth.New(home)
	if err != nil {
		t.Fatalf("reload: %v", err)
	}
	got, err := reloaded.Token(context.Background(), "cursor")
	if err != nil || got != fresh {
		t.Fatalf("stored = %+v (err %v), want %+v", got, err, fresh)
	}
}

func TestCredentialsSaveKeepsNewerLogin(t *testing.T) {
	t.Setenv("CURSOR_API_KEY", "")
	expires := time.Now().Add(time.Hour).Format(time.RFC3339)
	stored := auth.Credential{Type: "oauth", Access: "a1", Refresh: "r1", Expires: expires}
	creds, entry, home := newSaveTestCredentials(t, stored)
	fresh := auth.Credential{Type: "oauth", Access: "a2", Refresh: "r2", Expires: expires}
	if saveErr := creds.saveCredential(entry, "cursor", fresh); saveErr != nil {
		t.Fatalf("save: %v", saveErr)
	}
	newer := auth.Credential{Type: "oauth", Access: "a9", Refresh: "r9", Expires: expires}
	if loginErr := creds.store.SetCredential("cursor", newer); loginErr != nil {
		t.Fatalf("newer login: %v", loginErr)
	}
	stale := auth.Credential{Type: "oauth", Access: "a3", Refresh: "r3", Expires: expires}
	if saveErr := creds.saveCredential(entry, "cursor", stale); saveErr != nil {
		t.Fatalf("stale save: %v", saveErr)
	}
	if entry.stored != fresh {
		t.Fatalf("stale save mutated entry.stored to %+v", entry.stored)
	}
	reloaded, err := auth.New(home)
	if err != nil {
		t.Fatalf("reload: %v", err)
	}
	got, err := reloaded.Token(context.Background(), "cursor")
	if err != nil || got != newer {
		t.Fatalf("stored = %+v (err %v), want newer %+v", got, err, newer)
	}
}

func TestSourceReusesEntryWithMatchingSave(t *testing.T) {
	t.Setenv("CURSOR_API_KEY", "")
	expires := time.Now().Add(time.Hour).Format(time.RFC3339)
	stored := auth.Credential{Type: "oauth", Access: "a1", Refresh: "r1", Expires: expires}
	creds, entry, _ := newSaveTestCredentials(t, stored)
	entry.source = oauth.NewSource(oauth.Provider{ID: "cursor"}, nil, nil)
	entry.saving = stored
	creds.sources["cursor"] = entry
	if got := creds.source(context.Background(), "cursor"); got != entry.source {
		t.Fatal("source rebuilt while its save was in flight")
	}
}
