package auth_test

import (
	"context"
	"errors"
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
)

func writeAuthFile(t *testing.T, home, name, body string, mode os.FileMode) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(home, name), []byte(body), mode); err != nil {
		t.Fatalf("write %s: %v", name, err)
	}
}

func requireAuthError(t *testing.T, err error, want string) {
	t.Helper()
	if err == nil || err.Error() != want {
		t.Fatalf("error = %v, want %q", err, want)
	}
}

func readAuthJSON(t *testing.T, home string) string {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(home, "auth.json"))
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

func requirePrivateMode(t *testing.T, path string) {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if runtime.GOOS != "windows" && info.Mode().Perm() != 0o600 {
		t.Fatalf("mode = %o", info.Mode().Perm())
	}
}

func requireMask(t *testing.T, store *auth.Store, input, want string) {
	t.Helper()
	if got := store.Mask(input); got != want {
		t.Fatalf("mask = %q, want %q", got, want)
	}
}

func requireUnmask(t *testing.T, store *auth.Store, input, want string) {
	t.Helper()
	if got := store.Unmask(input); got != want {
		t.Fatalf("unmask = %q, want %q", got, want)
	}
}

func TestLoad_refusesGroupReadableFile(t *testing.T) {
	if runtime.GOOS == "windows" {
		requireLoadRefusesNonRegularFile(t)
		return
	}
	home := t.TempDir()
	writeAuthFile(t, home, "auth.json", `{}`, 0o640)
	_, err := auth.Load(home)
	requireAuthError(t, err, "auth.json must be a regular file with mode 0600")
}

func TestLoad_refusesSymlink(t *testing.T) {
	home := t.TempDir()
	writeAuthFile(t, home, "real.json", `{}`, 0o600)
	if err := os.Symlink(filepath.Join(home, "real.json"), filepath.Join(home, "auth.json")); err != nil {
		if runtime.GOOS != "windows" {
			t.Fatal(err)
		}
		requireLoadRefusesNonRegularFile(t)
		return
	}
	_, err := auth.Load(home)
	requireAuthError(t, err, "auth.json must be a regular file with mode 0600")
}

func requireLoadRefusesNonRegularFile(t *testing.T) {
	t.Helper()
	home := t.TempDir()
	if err := os.Mkdir(filepath.Join(home, "auth.json"), 0o700); err != nil {
		t.Fatal(err)
	}
	_, err := auth.Load(home)
	requireAuthError(t, err, "auth.json must be a regular file with mode 0600")
}

func TestStore_TokenEnvironmentOverrides(t *testing.T) {
	tests := []struct {
		provider string
		env      string
	}{
		{provider: "openai", env: "OPENAI_API_KEY"},
		{provider: "anthropic", env: "ANTHROPIC_API_KEY"},
		{provider: "google", env: "GEMINI_API_KEY"},
		{provider: "openrouter", env: "OPENROUTER_API_KEY"},
		{provider: "devin", env: "DEVIN_API_KEY"},
		{provider: "sandbox0", env: "SANDBOX0_TOKEN"},
	}
	for _, test := range tests {
		t.Run(test.provider, func(t *testing.T) {
			home := t.TempDir()
			writeAuthFile(t, home, "auth.json", fmt.Sprintf(`{%q:{"type":"api_key","key":"stored"}}`, test.provider), 0o600)
			t.Setenv(test.env, "from-environment")
			store, err := auth.Load(home)
			if err != nil {
				t.Fatal(err)
			}
			credential, err := store.Token(context.Background(), test.provider)
			if err != nil {
				t.Fatal(err)
			}
			if credential.Key != "from-environment" {
				t.Fatalf("credential = %+v", credential)
			}
		})
	}
}

func TestStore_TokenWithEnv(t *testing.T) {
	home := t.TempDir()
	writeAuthFile(t, home, "auth.json", `{"custom":{"type":"api_key","key":"stored"}}`, 0o600)
	t.Setenv("CUSTOM_PROVIDER_KEY", "custom-environment")
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	credential, err := store.TokenWithEnv(context.Background(), "custom", "CUSTOM_PROVIDER_KEY")
	if err != nil {
		t.Fatal(err)
	}
	if credential.Key != "custom-environment" {
		t.Fatalf("credential = %+v", credential)
	}
}

func TestStore_TokenWithoutCredentialNamesTheProvider(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "")
	store, err := auth.Load(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	_, err = store.Token(t.Context(), "openai")
	requireAuthError(t, err, "auth: no credential for openai")
	if !errors.Is(err, auth.ErrNoCredential) {
		t.Fatalf("error %v does not wrap auth.ErrNoCredential", err)
	}
}

func TestStore_SetAPIKeyPreservesUnknownEntries(t *testing.T) {
	home := t.TempDir()
	unknown := `"future":{"type":"future","key":"future-secret","extra":{"n":1}}`
	writeAuthFile(t, home, "auth.json", `{`+unknown+`}`, 0o600)
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	if setErr := store.SetAPIKey("openai", "openai-secret"); setErr != nil {
		t.Fatal(setErr)
	}
	if raw := readAuthJSON(t, home); !strings.Contains(raw, unknown) {
		t.Fatalf("unknown entry changed: %s", raw)
	}
	requirePrivateMode(t, filepath.Join(home, "auth.json"))
	if logoutErr := store.Logout("openai"); logoutErr != nil {
		t.Fatal(logoutErr)
	}
	raw := readAuthJSON(t, home)
	if !strings.Contains(raw, unknown) || strings.Contains(raw, "openai-secret") {
		t.Fatalf("logout changed unknown entry: %s", raw)
	}
}

func TestStore_SetCredentialRoundTrip(t *testing.T) {
	home := t.TempDir()
	t.Setenv("CHATGPT_API_KEY", "")
	writeAuthFile(t, home, "auth.json", `{"chatgpt":{"type":"oauth","access":"stale-access","extra":1}}`, 0o600)
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	want := auth.Credential{Type: "oauth", Access: "fresh-access", Refresh: "fresh-refresh", Expires: "2026-11-08T10:00:00Z", Account: "acct-1"}
	if setErr := store.SetCredential("chatgpt", want); setErr != nil {
		t.Fatal(setErr)
	}
	requirePrivateMode(t, filepath.Join(home, "auth.json"))
	if raw := readAuthJSON(t, home); strings.Contains(raw, "stale-access") || strings.Contains(raw, "extra") {
		t.Fatalf("stale raw entry survived: %s", raw)
	}
	reloaded, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	got, err := reloaded.Token(t.Context(), "chatgpt")
	if err != nil {
		t.Fatal(err)
	}
	if got != want {
		t.Fatalf("credential = %+v, want %+v", got, want)
	}
}

func loadStore(t *testing.T, home string) *auth.Store {
	t.Helper()
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	return store
}

func TestStore_TwoStoresKeepEachOthersCredentials(t *testing.T) {
	home := t.TempDir()
	t.Setenv("OPENAI_API_KEY", "")
	t.Setenv("CHATGPT_API_KEY", "")
	writeAuthFile(t, home, "auth.json", `{"chatgpt":{"type":"oauth","access":"old-access-token","refresh":"old-refresh-token"}}`, 0o600)
	storeA, storeB := loadStore(t, home), loadStore(t, home)
	if err := storeA.SetAPIKey("openai", "key-from-store-a"); err != nil {
		t.Fatal(err)
	}
	refreshed := auth.Credential{Type: "oauth", Access: "new-access-token", Refresh: "new-refresh-token"}
	if err := storeB.SetCredential("chatgpt", refreshed); err != nil {
		t.Fatal(err)
	}
	raw := readAuthJSON(t, home)
	if !strings.Contains(raw, "key-from-store-a") || !strings.Contains(raw, "new-refresh-token") || strings.Contains(raw, "old-refresh-token") {
		t.Fatalf("auth.json lost a committed credential or kept the rotated token: %s", raw)
	}
	if masked := storeB.Mask("sent key-from-store-a"); strings.Contains(masked, "key-from-store-a") {
		t.Fatalf("store B did not mask store A's key: %q", masked)
	}
	got, err := storeA.Token(t.Context(), "chatgpt")
	if err != nil || got != refreshed {
		t.Fatalf("store A credential = %+v, %v; want %+v", got, err, refreshed)
	}
}

func TestStore_MaskFailsClosedWhenAuthCannotBeReread(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("mode bits do not make auth.json insecure on Windows")
	}
	home := t.TempDir()
	store := loadStore(t, home)
	if err := store.SetAPIKey("openai", "secret-before-change"); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(filepath.Join(home, "auth.json")); err != nil {
		t.Fatal(err)
	}
	writeAuthFile(t, home, "auth.json", `{"openai":{"type":"api_key","key":"secret-after-change"}}`, 0o644)
	requireMask(t, store, "secret-after-change", "secret masking failed: auth.json could not be reread")
	if store.MaskError() == nil {
		t.Fatal("reload failure was not recorded")
	}
	if err := os.Chmod(filepath.Join(home, "auth.json"), 0o600); err != nil {
		t.Fatal(err)
	}
	if got := store.Mask("secret-after-change"); strings.Contains(got, "secret-after-change") || store.MaskError() != nil {
		t.Fatalf("mask after repair = %q, %v", got, store.MaskError())
	}
}

func TestStore_ConcurrentWritersKeepEveryProvider(t *testing.T) {
	home := t.TempDir()
	const writers = 8
	stores := make([]*auth.Store, writers)
	for i := range stores {
		store, err := auth.Load(home)
		if err != nil {
			t.Fatal(err)
		}
		stores[i] = store
	}
	errs := make(chan error, writers)
	for i, store := range stores {
		go func() { errs <- store.SetAPIKey(fmt.Sprintf("provider%d", i), fmt.Sprintf("secret-value-%d", i)) }()
	}
	for range writers {
		if err := <-errs; err != nil {
			t.Fatal(err)
		}
	}
	raw := readAuthJSON(t, home)
	for i := range writers {
		if want := fmt.Sprintf("secret-value-%d", i); !strings.Contains(raw, want) {
			t.Fatalf("auth.json lost %q: %s", want, raw)
		}
	}
}

func TestStore_MaskPlaceholders(t *testing.T) {
	home := t.TempDir()
	writeAuthFile(t, home, "auth.json", `{"openai":{"type":"api_key","key":"stored-secret"}}`, 0o600)
	t.Setenv("OTHER_API_KEY", "environment-secret")
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	masked := store.Mask("stored-secret environment-secret")
	if strings.Contains(masked, "stored-secret") || strings.Contains(masked, "environment-secret") {
		t.Fatalf("secrets remain in %q", masked)
	}
	if !strings.Contains(masked, "{{secret:auth:openai}}") || !strings.Contains(masked, "{{secret:OTHER_API_KEY}}") {
		t.Fatalf("wrong placeholders in %q", masked)
	}
	if got := store.Unmask(masked); got != "stored-secret environment-secret" {
		t.Fatalf("unmask = %q", got)
	}
	if got := store.Unmask("{{secret:unknown}}"); got != "{{secret:unknown}}" {
		t.Fatalf("unknown placeholder changed: %q", got)
	}
}

func TestStore_MaskUsesLongestValueFirst(t *testing.T) {
	home := t.TempDir()
	writeAuthFile(t, home, "auth.json", `{"openai":{"type":"api_key","key":"abcdefghijkl"}}`, 0o600)
	t.Setenv("OVERLAP_API_KEY", "abcdefgh")
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	masked := store.Mask("abcdefghijkl")
	if masked != "{{secret:auth:openai}}" {
		t.Fatalf("masked = %q", masked)
	}
	if got := store.Unmask(masked); got != "abcdefghijkl" {
		t.Fatalf("unmask = %q", got)
	}
}

func TestStore_MaskDoesNotMaskSevenBytes(t *testing.T) {
	home := t.TempDir()
	writeAuthFile(t, home, "auth.json", `{"openai":{"type":"api_key","key":"tiny123"}}`, 0o600)
	t.Setenv("SHORT_API_KEY", "short12")
	store, err := auth.NewWithPatterns(home, []string{"tinyabc"})
	if err != nil {
		t.Fatal(err)
	}
	const input = "tiny123 short12 tinyabc"
	if got := store.Mask(input); got != input {
		t.Fatalf("short values masked: %q", got)
	}
}

func TestStore_MaskPatternPersists(t *testing.T) {
	home := t.TempDir()
	store, err := auth.NewWithPatterns(home, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	requireMask(t, store, "token-abcdefgh", "{{secret:pattern0:0}}")
	requireMask(t, store, "token-abcdefgh", "{{secret:pattern0:0}}")
	requireMask(t, store, "token-ijklmnop", "{{secret:pattern0:1}}")

	reloaded, err := auth.NewWithPatterns(home, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	requireMask(t, reloaded, "token-abcdefgh", "{{secret:pattern0:0}}")
	requireUnmask(t, reloaded, "{{secret:pattern0:0}} {{secret:pattern0:1}}", "token-abcdefgh token-ijklmnop")
	requirePrivateMode(t, filepath.Join(home, "secrets.json"))
}

func TestStore_MaskDoesNotReplaceInsidePlaceholders(t *testing.T) {
	home := t.TempDir()
	writeAuthFile(t, home, "auth.json", `{"openai":{"type":"oauth","access":"{{secret:auth:","refresh":"refresh-secret"}}`, 0o600)
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	const input = "{{secret:auth: refresh-secret"
	masked := store.Mask(input)
	if masked == input || strings.Contains(masked, "refresh-secret") {
		t.Fatalf("masking failed: %q", masked)
	}
	if got := store.Unmask(masked); got != input {
		t.Fatalf("round trip = %q, want %q", got, input)
	}
}

func TestStore_MaskOAuthFieldsRemainDistinct(t *testing.T) {
	home := t.TempDir()
	writeAuthFile(t, home, "auth.json", `{"provider":{"type":"oauth","key":"primary-secret","access":"access-secret","refresh":"refresh-secret"}}`, 0o600)
	store, err := auth.Load(home)
	if err != nil {
		t.Fatal(err)
	}
	const input = "primary-secret access-secret refresh-secret"
	const masked = "{{secret:auth:provider}} {{secret:auth:provider:access}} {{secret:auth:provider:refresh}}"
	requireMask(t, store, input, masked)
	requireUnmask(t, store, masked, input)
}

func TestStore_MaskPersistenceFailureFailsClosedAndRetries(t *testing.T) {
	home := t.TempDir()
	store, err := auth.NewWithPatterns(home, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	if removeErr := os.RemoveAll(home); removeErr != nil {
		t.Fatal(removeErr)
	}
	if writeErr := os.WriteFile(home, []byte("not a directory"), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	requireMask(t, store, "token-abcdefgh", "secret masking failed: pattern state was not persisted")
	if store.MaskError() == nil {
		t.Fatal("persistence failure was not recorded")
	}
	if removeErr := os.Remove(home); removeErr != nil {
		t.Fatal(removeErr)
	}
	if mkdirErr := os.Mkdir(home, 0o700); mkdirErr != nil {
		t.Fatal(mkdirErr)
	}
	requireMask(t, store, "token-abcdefgh", "{{secret:pattern0:0}}")
	if store.MaskError() != nil {
		t.Fatalf("retry did not clear the error: %v", store.MaskError())
	}
	reloaded, err := auth.NewWithPatterns(home, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	requireUnmask(t, reloaded, "{{secret:pattern0:0}}", "token-abcdefgh")
}

func TestStore_MaskProperty(t *testing.T) {
	rng := rand.New(rand.NewPCG(42, 42))
	const alphabet = "abcdefghijklmnopqrstuvwxyz0123456789"
	randomText := func(min, max int) string {
		n := min + rng.IntN(max-min+1)
		var b strings.Builder
		b.Grow(n)
		for b.Len() < n {
			b.WriteByte(alphabet[rng.IntN(len(alphabet))])
		}
		return b.String()
	}
	for iteration := range 100 {
		home := t.TempDir()
		stored := randomText(8, 40)
		environment := randomText(8, 40)
		pattern := "token-" + randomText(8, 20)
		writeAuthFile(t, home, "auth.json", fmt.Sprintf(`{"openai":{"type":"api_key","key":%q}}`, stored), 0o600)
		t.Setenv("PROPERTY_API_KEY", environment)
		store, err := auth.NewWithPatterns(home, []string{`token-[a-z0-9]{8,}`})
		if err != nil {
			t.Fatal(err)
		}
		input := randomText(0, 20) + stored + randomText(1, 20) + environment + randomText(1, 20) + pattern + randomText(0, 20)
		masked := store.Mask(input)
		for _, secret := range []string{stored, environment, pattern} {
			if strings.Contains(masked, secret) {
				t.Fatalf("iteration %d: secret %q remains in %q", iteration, secret, masked)
			}
		}
		if got := store.Unmask(masked); got != input {
			t.Fatalf("iteration %d: round trip = %q, want %q", iteration, got, input)
		}
	}
}
