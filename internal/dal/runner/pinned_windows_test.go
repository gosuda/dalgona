//go:build windows

package runner

import (
	"errors"
	"io"
	"os/exec"
	"path/filepath"
	"testing"
)

func createJunction(t *testing.T, link, target string) {
	t.Helper()
	if out, err := exec.Command("cmd", "/C", "mklink", "/J", link, target).CombinedOutput(); err != nil {
		t.Fatalf("mklink /J %s %s: %v: %s", link, target, err, out)
	}
}

func readNoFollow(t *testing.T, path string) string {
	t.Helper()
	file, err := openNoFollow(path)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = file.Close() }()
	data, err := io.ReadAll(file)
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

func TestOpenPinnedRefusesJunctionComponent(t *testing.T) {
	root, secret := pinnedRoot(t)
	createJunction(t, filepath.Join(root, "evil"), filepath.Dir(secret))
	if _, err := OpenPinned(root, "evil/secret.txt"); !errors.Is(err, ErrPinnedSymlink) {
		t.Fatalf("junction component error = %v, want ErrPinnedSymlink", err)
	}
	if _, err := OpenPinned(root, "evil"); !errors.Is(err, ErrPinnedSymlink) {
		t.Fatalf("junction leaf error = %v, want ErrPinnedSymlink", err)
	}
}

func TestOpenNoFollowRefusesOnlyFinalJunction(t *testing.T) {
	root, secret := pinnedRoot(t)
	link := filepath.Join(root, "link")
	createJunction(t, link, filepath.Dir(secret))
	if _, err := openNoFollow(link); !errors.Is(err, ErrPinnedSymlink) {
		t.Fatalf("final junction error = %v, want ErrPinnedSymlink", err)
	}
	if got := readNoFollow(t, filepath.Join(link, "secret.txt")); got != "top-secret\n" {
		t.Fatalf("read through a junction parent = %q, want the file content", got)
	}
	if got := readNoFollow(t, filepath.Join(root, "sub", "ok.txt")); got != "fine\n" {
		t.Fatalf("plain read = %q, want the file content", got)
	}
}
