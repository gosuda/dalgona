package client

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"testing"
)

func TestBuildMatchesExecutableDigest(t *testing.T) {
	want := ownDigest(t, nil)
	if got := Build(); got != want {
		t.Fatalf("Build() = %q, want %q", got, want)
	}
	if got := Build(); got != want {
		t.Fatalf("second Build() = %q, want %q", got, want)
	}
	if altered := ownDigest(t, []byte{0}); altered == Build() {
		t.Fatal("digest of an altered executable equals Build(), so the negative control cannot fail")
	}
}

func ownDigest(t *testing.T, extra []byte) string {
	t.Helper()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	if resolved, resolveErr := filepath.EvalSymlinks(exe); resolveErr == nil {
		exe = resolved
	}
	data, err := os.ReadFile(exe)
	if err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(append(data, extra...))
	return hex.EncodeToString(digest[:])[:16]
}

func TestResolveBackendFollowsSymlinksAndHashesTheTarget(t *testing.T) {
	dir := t.TempDir()
	target := filepath.Join(dir, "backend")
	if err := os.WriteFile(target, []byte("daemon bytes"), 0o700); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(dir, "link")
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("symlink: %v", err)
	}
	backend, err := ResolveBackend(link)
	if err != nil {
		t.Fatal(err)
	}
	resolved, err := filepath.EvalSymlinks(target)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256([]byte("daemon bytes"))
	if backend.Path != resolved || backend.Build != hex.EncodeToString(sum[:])[:16] {
		t.Fatalf("backend = %+v, want %s with the digest of its bytes", backend, resolved)
	}
	if _, err := ResolveBackend(filepath.Join(dir, "missing")); err == nil {
		t.Fatal("missing backend resolved")
	}
}
