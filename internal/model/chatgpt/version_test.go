package chatgpt

import (
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	json "encoding/json/v2"
)

func TestRegistryRedirectDoesNotLeaveDiscoveryEndpoint(t *testing.T) {
	registry, models := okRegistry("0.153.0"), okModels()
	registry.update(func(b *backend) {
		b.status, b.location = http.StatusFound, "https://evil.invalid/registry"
	})
	s := newSite(t, registry, models)
	account := "acct-A"
	_, err := New(s.config(t.TempDir(), &account)).Models(t.Context())
	if err == nil || !strings.Contains(err.Error(), "registry answered status 302") {
		t.Fatalf("Models error = %v, want the original registry redirect refusal", err)
	}
	if registry.view().hits != 1 || models.view().hits != 0 || s.blocked.Load() != 0 {
		t.Fatal("discovery followed the redirect or requested models without a released version")
	}
}

type versionOnDisk struct {
	Version   string    `json:"version"`
	ETag      string    `json:"etag"`
	CheckedAt time.Time `json:"checked_at"`
}

func seedRaw(t *testing.T, dir, content string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, "codex-client-version.json"), []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
}

func seedVersion(t *testing.T, dir string, entry versionOnDisk) {
	t.Helper()
	data, err := json.Marshal(entry)
	if err != nil {
		t.Fatal(err)
	}
	seedRaw(t, dir, string(data))
}

func readVersionFile(t *testing.T, dir string) versionOnDisk {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(dir, "codex-client-version.json"))
	if err != nil {
		t.Fatal(err)
	}
	var entry versionOnDisk
	err = json.Unmarshal(raw, &entry)
	if err != nil {
		t.Fatalf("decode version file: %v: %s", err, raw)
	}
	return entry
}

func sameEntry(got, want versionOnDisk) bool {
	return got.Version == want.Version && got.ETag == want.ETag && got.CheckedAt.Equal(want.CheckedAt)
}

func TestClientVersionCache(t *testing.T) {
	registry, models := okRegistry("0.150.0"), okModels()
	registry.update(func(b *backend) { b.etag = `"e1"` })
	s := newSite(t, registry, models)
	dir := t.TempDir()
	account := "acct-A"
	clk := newClock()
	start := clk.now
	provider := New(s.config(dir, &account))
	provider.now = clk.Now

	call := func(name string, wantHits int, wantMatch, wantQuery string) {
		t.Helper()
		expectIDs(t, provider, name, liveIDs())
		npm, list := registry.view(), models.view()
		if npm.hits != wantHits || npm.match != wantMatch || list.query != wantQuery {
			t.Fatalf("%s: registry hits %d with If-None-Match %q and model query %q, want %d, %q and %q",
				name, npm.hits, npm.match, list.query, wantHits, wantMatch, wantQuery)
		}
	}

	call("cold cache looks up once", 1, "", "0.150.0")
	want := versionOnDisk{Version: "0.150.0", ETag: `"e1"`, CheckedAt: start}
	if got := readVersionFile(t, dir); !sameEntry(got, want) {
		t.Fatalf("version file = %+v, want %+v", got, want)
	}

	clk.advance(4*time.Hour - time.Second)
	call("one second under four hours uses the cache", 1, "", "0.150.0")

	clk.advance(time.Second)
	registry.update(func(b *backend) { b.status, b.body = http.StatusNotModified, "" })
	call("four hours revalidates with the stored tag", 2, `"e1"`, "0.150.0")
	want.CheckedAt = start.Add(4 * time.Hour)
	if got := readVersionFile(t, dir); !sameEntry(got, want) {
		t.Fatalf("version file after 304 = %+v, want %+v", got, want)
	}

	clk.advance(3 * time.Hour)
	call("a renewed entry is fresh again", 2, `"e1"`, "0.150.0")

	clk.advance(time.Hour)
	registry.update(func(b *backend) {
		b.status, b.body, b.etag = http.StatusOK, `{"version":"0.151.0"}`, `"e2"`
	})
	call("a new release replaces the version", 3, `"e1"`, "0.151.0")
	want = versionOnDisk{Version: "0.151.0", ETag: `"e2"`, CheckedAt: start.Add(8 * time.Hour)}
	if got := readVersionFile(t, dir); !sameEntry(got, want) {
		t.Fatalf("version file after release = %+v, want %+v", got, want)
	}
}

func TestClientVersionRejectsUnreleased(t *testing.T) {
	rejected := []struct {
		name   string
		status int
		body   string
	}{
		{"beta tag", http.StatusOK, `{"version":"1.2.3-beta.1"}`},
		{"zero version", http.StatusOK, `{"version":"0.0.0"}`},
		{"two parts", http.StatusOK, `{"version":"1.2"}`},
		{"v prefix", http.StatusOK, `{"version":"v1.2.3"}`},
		{"four parts", http.StatusOK, `{"version":"1.2.3.4"}`},
		{"empty version", http.StatusOK, `{"version":""}`},
		{"no version member", http.StatusOK, `{}`},
		{"not json", http.StatusOK, `<html>`},
		{"server error", http.StatusServiceUnavailable, ``},
		{"not modified without a cache", http.StatusNotModified, ``},
	}
	for _, tc := range rejected {
		t.Run(tc.name, func(t *testing.T) {
			checkRejectedVersion(t, tc.status, tc.body)
		})
	}
	for _, version := range []string{"1.2.3", "0.0.1", "10.0.0", "0.153.0"} {
		t.Run("accepts "+version, func(t *testing.T) {
			checkAcceptedVersion(t, version)
		})
	}
}

func checkRejectedVersion(t *testing.T, status int, body string) {
	t.Helper()
	registry, models := &backend{status: status, body: body, etag: `"x"`}, okModels()
	s := newSite(t, registry, models)
	dir := t.TempDir()
	account := "acct-A"
	_, err := New(s.config(dir, &account)).Models(t.Context())
	if err == nil || !strings.HasPrefix(err.Error(), "chatgpt: client version is unavailable: ") {
		t.Fatalf("err = %v, want the unavailable-version error", err)
	}
	if hits := models.view().hits; hits != 0 {
		t.Fatalf("model list was requested %d times without a released version", hits)
	}
	if entries, readErr := os.ReadDir(dir); readErr != nil || len(entries) != 0 {
		t.Fatalf("cache directory = %v, %v, want it untouched", entries, readErr)
	}
}

func checkAcceptedVersion(t *testing.T, version string) {
	t.Helper()
	models := okModels()
	s := newSite(t, okRegistry(version), models)
	dir := t.TempDir()
	account := "acct-A"
	expectIDs(t, New(s.config(dir, &account)), version, liveIDs())
	if query := models.view().query; query != version {
		t.Fatalf("model query = %q, want %q", query, version)
	}
	if got := readVersionFile(t, dir); got.Version != version {
		t.Fatalf("version file = %+v, want %s", got, version)
	}
}

func TestClientVersionRetainsStaleCacheWhenLookupFails(t *testing.T) {
	failures := []struct {
		name   string
		status int
		body   string
	}{
		{"server error", http.StatusServiceUnavailable, ``},
		{"zero version", http.StatusOK, `{"version":"0.0.0"}`},
		{"prerelease", http.StatusOK, `{"version":"2.0.0-rc.1"}`},
	}
	for _, tc := range failures {
		t.Run(tc.name, func(t *testing.T) {
			registry, models := &backend{status: tc.status, body: tc.body}, okModels()
			s := newSite(t, registry, models)
			dir := t.TempDir()
			account := "acct-A"
			clk := newClock()
			old := versionOnDisk{Version: "0.140.0", ETag: `"old"`, CheckedAt: clk.now.Add(-5 * time.Hour)}
			seedVersion(t, dir, old)
			provider := New(s.config(dir, &account))
			provider.now = clk.Now
			expectIDs(t, provider, "stale cache", liveIDs())
			if npm := registry.view(); npm.hits != 1 || npm.match != `"old"` {
				t.Fatalf("registry saw %+v, want one revalidation with the stored tag", npm)
			}
			if query := models.view().query; query != "0.140.0" {
				t.Fatalf("model query = %q, want the retained 0.140.0", query)
			}
			if got := readVersionFile(t, dir); !sameEntry(got, old) {
				t.Fatalf("version file = %+v, want it unchanged %+v", got, old)
			}
		})
	}
}

func TestClientVersionIgnoresInvalidCacheEntries(t *testing.T) {
	now := time.Now()
	invalid := map[string]string{
		"zero version": `{"version":"0.0.0","checked_at":"` + now.Format(time.RFC3339) + `"}`,
		"prerelease":   `{"version":"1.2.3-beta","checked_at":"` + now.Format(time.RFC3339) + `"}`,
		"not json":     `{not json`,
		"empty":        ``,
	}
	for name, content := range invalid {
		t.Run(name, func(t *testing.T) {
			registry, models := okRegistry("0.152.0"), okModels()
			s := newSite(t, registry, models)
			dir := t.TempDir()
			account := "acct-A"
			seedRaw(t, dir, content)
			expectIDs(t, New(s.config(dir, &account)), name, liveIDs())
			if npm := registry.view(); npm.hits != 1 || npm.match != "" {
				t.Fatalf("registry saw %+v, want one plain lookup", npm)
			}
			if query := models.view().query; query != "0.152.0" {
				t.Fatalf("model query = %q, want 0.152.0", query)
			}
		})
	}
	t.Run("valid fresh entry skips the lookup", func(t *testing.T) {
		registry, models := okRegistry("0.152.0"), okModels()
		s := newSite(t, registry, models)
		dir := t.TempDir()
		account := "acct-A"
		seedVersion(t, dir, versionOnDisk{Version: "0.141.0", CheckedAt: now})
		expectIDs(t, New(s.config(dir, &account)), "fresh", liveIDs())
		if hits := registry.view().hits; hits != 0 {
			t.Fatalf("registry hits = %d, want 0", hits)
		}
		if query := models.view().query; query != "0.141.0" {
			t.Fatalf("model query = %q, want 0.141.0", query)
		}
	})
}

func TestClientVersionCapsRegistryBody(t *testing.T) {
	head, tail := `{"version":"1.2.3","pad":"`, `"}`
	body := func(size int) string {
		return head + strings.Repeat("a", size-len(head)-len(tail)) + tail
	}
	t.Run("exactly the limit", func(t *testing.T) {
		registry, models := &backend{status: http.StatusOK, body: body(lookupLimit)}, okModels()
		s := newSite(t, registry, models)
		account := "acct-A"
		expectIDs(t, New(s.config(t.TempDir(), &account)), "limit", liveIDs())
		if query := models.view().query; query != "1.2.3" {
			t.Fatalf("model query = %q, want 1.2.3", query)
		}
	})
	t.Run("one byte over", func(t *testing.T) {
		registry, models := &backend{status: http.StatusOK, body: body(lookupLimit + 1)}, okModels()
		s := newSite(t, registry, models)
		account := "acct-A"
		_, err := New(s.config(t.TempDir(), &account)).Models(t.Context())
		if err == nil || !strings.HasPrefix(err.Error(), "chatgpt: client version is unavailable: ") || models.view().hits != 0 {
			t.Fatalf("err = %v, want the unavailable-version error before any model request", err)
		}
	})
}
