package auth_test

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
)

func newPatternMaskStore(t *testing.T, home string) *auth.Store {
	t.Helper()
	store, err := auth.NewWithPatterns(home, []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	return store
}

func readMaskSecretsFile(t *testing.T, home string) string {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(home, "secrets.json"))
	if err != nil {
		t.Fatal(err)
	}
	return string(raw)
}

func checkMaskUnmask(t *testing.T, store *auth.Store, masked, want, label string) {
	t.Helper()
	if got := store.Unmask(masked); got != want {
		t.Fatalf("%s = %q, want %q", label, got, want)
	}
}

func TestStore_MaskConcurrentPatternSecretsKeepBoth(t *testing.T) {
	home := t.TempDir()
	storeA := newPatternMaskStore(t, home)
	storeB := newPatternMaskStore(t, home)
	const secretA = "token-aaaaaaaa"
	const secretB = "token-bbbbbbbb"
	var maskedA, maskedB string
	var wg sync.WaitGroup
	wg.Add(2)
	go func() {
		defer wg.Done()
		maskedA = storeA.Mask(secretA)
	}()
	go func() {
		defer wg.Done()
		maskedB = storeB.Mask(secretB)
	}()
	wg.Wait()
	if maskedA == maskedB {
		t.Fatalf("placeholders collide: %q", maskedA)
	}
	raw := readMaskSecretsFile(t, home)
	if !strings.Contains(raw, secretA) || !strings.Contains(raw, secretB) {
		t.Fatalf("secrets.json lost a mapping: %s", raw)
	}
	checkMaskUnmask(t, storeA, maskedB, secretB, "storeA unmask")
	checkMaskUnmask(t, storeB, maskedA, secretA, "storeB unmask")
	reloaded := newPatternMaskStore(t, home)
	checkMaskUnmask(t, reloaded, maskedA, secretA, "reloaded unmask A")
	checkMaskUnmask(t, reloaded, maskedB, secretB, "reloaded unmask B")
}
