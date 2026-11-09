//go:build linux || darwin

package runner

import (
	"os"
	"path/filepath"
	"testing"
)

func TestResolveRoots_resolvesSymlinkedRoot(t *testing.T) {
	dir := t.TempDir()
	real := filepath.Join(dir, "real")
	if err := os.Mkdir(real, 0o700); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(dir, "link")
	if err := os.Symlink(real, link); err != nil {
		t.Fatal(err)
	}
	got, err := resolveRoots([]string{link})
	if err != nil {
		t.Fatal(err)
	}
	want, err := filepath.EvalSymlinks(real)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, root := range got {
		if root == want {
			found = true
		}
	}
	if !found {
		t.Fatalf("resolveRoots = %q, want entry %q", got, want)
	}
}

func TestResolveRoots_cleansDotDot(t *testing.T) {
	dir := t.TempDir()
	sub := filepath.Join(dir, "sub")
	if err := os.Mkdir(sub, 0o700); err != nil {
		t.Fatal(err)
	}
	dotdot := filepath.Join(dir, "other", "..", "sub")
	got, err := resolveRoots([]string{dotdot})
	if err != nil {
		t.Fatal(err)
	}
	want, err := filepath.EvalSymlinks(sub)
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for _, root := range got {
		if root == want {
			found = true
		}
	}
	if !found {
		t.Fatalf("resolveRoots = %q, want entry %q", got, want)
	}
}

func TestResolveRoots_rejectsMissingRoot(t *testing.T) {
	missing := filepath.Join(t.TempDir(), "does-not-exist")
	if _, err := resolveRoots([]string{missing}); err == nil {
		t.Fatal("resolveRoots accepted a missing root")
	}
}
