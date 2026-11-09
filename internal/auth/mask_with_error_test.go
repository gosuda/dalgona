package auth_test

import (
	"os"
	"strings"
	"sync"
	"testing"
)

func breakPatternHome(t *testing.T, dir string) {
	t.Helper()
	if err := os.RemoveAll(dir); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(dir, []byte("not a directory"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = os.Remove(dir)
		_ = os.Mkdir(dir, 0o700)
	})
}

func TestStore_MaskWithErrorFailsClosed(t *testing.T) {
	home := t.TempDir()
	store := newPatternMaskStore(t, home, `token-[a-z0-9]{8,}`)
	masked, err := store.MaskWithError("prefix token-abcdefgh suffix")
	if err != nil {
		t.Fatalf("MaskWithError = %q, %v, want masked text", masked, err)
	}
	if !strings.Contains(masked, "{{secret:") {
		t.Fatalf("MaskWithError = %q, want placeholder", masked)
	}
	breakPatternHome(t, home)
	masked, err = store.MaskWithError("prefix token-abcdefgh suffix")
	if err == nil {
		t.Fatalf("MaskWithError = %q, want error", masked)
	}
	if masked != "" {
		t.Fatalf("MaskWithError = %q, want empty text on failure", masked)
	}
}

func TestStore_MaskWithErrorConcurrentFailure(t *testing.T) {
	home := t.TempDir()
	store := newPatternMaskStore(t, home, `token-[a-z0-9]{8,}`)
	breakPatternHome(t, home)
	const callers = 8
	errs := make([]error, callers)
	masked := make([]string, callers)
	var wg sync.WaitGroup
	for i := range callers {
		wg.Go(func() {
			m, err := store.MaskWithError("prefix token-abcdefgh suffix")
			masked[i], errs[i] = m, err
		})
	}
	wg.Wait()
	for i := range callers {
		if errs[i] == nil {
			t.Fatalf("caller %d: MaskWithError = %q, want error", i, masked[i])
		}
		if masked[i] != "" {
			t.Fatalf("caller %d: MaskWithError = %q, want empty text on failure", i, masked[i])
		}
	}
}
