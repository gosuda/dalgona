package runner

import (
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"testing"
)

func pinnedRoot(t *testing.T) (string, string) {
	t.Helper()
	root := t.TempDir()
	outside := t.TempDir()
	secret := filepath.Join(outside, "secret.txt")
	if err := os.WriteFile(secret, []byte("top-secret\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Join(root, "sub"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "sub", "ok.txt"), []byte("fine\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	return root, secret
}

func openPinnedStat(t *testing.T, root, rel string) (*os.File, fs.FileInfo) {
	t.Helper()
	file, err := OpenPinned(root, rel)
	if err != nil {
		t.Fatal(err)
	}
	info, err := file.Stat()
	if err != nil {
		_ = file.Close()
		t.Fatal(err)
	}
	return file, info
}

func readPinnedAll(t *testing.T, root, rel string) ([]byte, fs.FileInfo) {
	t.Helper()
	file, info := openPinnedStat(t, root, rel)
	defer func() { _ = file.Close() }()
	data, err := io.ReadAll(file)
	if err != nil {
		t.Fatal(err)
	}
	return data, info
}

func TestOpenPinnedReadsAndStats(t *testing.T) {
	root, _ := pinnedRoot(t)
	data, info := readPinnedAll(t, root, "sub/ok.txt")
	if string(data) != "fine\n" {
		t.Fatalf("pinned read = %q, want file content", data)
	}
	if info.IsDir() || info.Size() != int64(len(data)) {
		t.Fatalf("pinned stat = dir:%v size:%d, want regular size %d", info.IsDir(), info.Size(), len(data))
	}
	file, info := openPinnedStat(t, root, "sub")
	_ = file.Close()
	if !info.IsDir() {
		t.Fatalf("pinned open of dir did not stat as directory")
	}
}

func TestOpenPinnedRefusesSymlinks(t *testing.T) {
	root, secret := pinnedRoot(t)
	link := filepath.Join(root, "link.txt")
	if err := os.Symlink(secret, link); err != nil {
		t.Skipf("symlink creation requires additional privileges: %v", err)
	}
	if _, err := OpenPinned(root, "link.txt"); !errors.Is(err, ErrPinnedSymlink) {
		t.Fatalf("leaf symlink error = %v, want ErrPinnedSymlink", err)
	}
	if err := os.Symlink(filepath.Dir(secret), filepath.Join(root, "evil")); err != nil {
		t.Skipf("symlink creation requires additional privileges: %v", err)
	}
	if _, err := OpenPinned(root, "evil/secret.txt"); !errors.Is(err, ErrPinnedSymlink) {
		t.Fatalf("parent symlink error = %v, want ErrPinnedSymlink", err)
	}
}

func TestOpenPinnedRefusesEscape(t *testing.T) {
	root, _ := pinnedRoot(t)
	for _, rel := range []string{"../escape", "sub/../../escape", "/abs"} {
		if _, err := OpenPinned(root, rel); !errors.Is(err, ErrPinnedEscape) {
			t.Fatalf("escape %q error = %v, want ErrPinnedEscape", rel, err)
		}
	}
}

func TestOpenPinnedMissing(t *testing.T) {
	root, _ := pinnedRoot(t)
	if _, err := OpenPinned(root, "sub/missing.txt"); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("missing error = %v, want ErrNotExist", err)
	}
}
