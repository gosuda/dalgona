package search

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func writeSymlinkEscapeFixture(t *testing.T) (cwd, outside string) {
	t.Helper()
	cwd = t.TempDir()
	outside = t.TempDir()
	if err := os.WriteFile(filepath.Join(outside, "secret.txt"), []byte("needle-secret\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(outside, "sub"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(outside, "sub", "deep.txt"), []byte("needle-deep\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(cwd, "ok.txt"), []byte("nothing\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return cwd, outside
}

func TestSearchScopeFileSymlinkRejected(t *testing.T) {
	cwd, outside := writeSymlinkEscapeFixture(t)
	if err := os.Symlink(filepath.Join(outside, "secret.txt"), filepath.Join(cwd, "link.txt")); err != nil {
		t.Fatal(err)
	}
	_, err := execute(cwd, args{Mode: "grep", Pattern: "needle", Path: "link.txt", Limit: 100})
	if err == nil || !strings.Contains(err.Error(), "escapes the workspace via symlink") {
		t.Fatalf("scope file symlink err = %v, want workspace escape rejection", err)
	}
}

func TestSearchScopeNestedSymlinkRejected(t *testing.T) {
	cwd, outside := writeSymlinkEscapeFixture(t)
	if err := os.Symlink(outside, filepath.Join(cwd, "notes")); err != nil {
		t.Fatal(err)
	}
	_, err := execute(cwd, args{Mode: "grep", Pattern: "needle", Path: "notes/sub", Limit: 100})
	if err == nil || !strings.Contains(err.Error(), "escapes the workspace via symlink") {
		t.Fatalf("nested symlink scope err = %v, want workspace escape rejection", err)
	}
}

func TestSearchScanSkipsSymlinkFile(t *testing.T) {
	cwd, outside := writeSymlinkEscapeFixture(t)
	if err := os.Symlink(filepath.Join(outside, "secret.txt"), filepath.Join(cwd, "link.txt")); err != nil {
		t.Fatal(err)
	}
	result, err := execute(cwd, args{Mode: "grep", Pattern: "needle", Limit: 100})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(result.Text, "needle-secret") {
		t.Fatalf("workspace grep leaked outside content: %q", result.Text)
	}
}
