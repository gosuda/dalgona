package config

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

func TestFileBuildHashesFileBytes(t *testing.T) {
	path := filepath.Join(t.TempDir(), "backend")
	if err := os.WriteFile(path, []byte("daemon bytes"), 0o700); err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256([]byte("daemon bytes"))
	if got, err := FileBuild(path); err != nil || got != hex.EncodeToString(sum[:])[:16] {
		t.Fatalf("FileBuild = %q, %v; want the first 16 hex digits of SHA-256", got, err)
	}
	if _, err := FileBuild(filepath.Join(t.TempDir(), "missing")); err == nil {
		t.Fatal("FileBuild accepted a missing file")
	}
}
