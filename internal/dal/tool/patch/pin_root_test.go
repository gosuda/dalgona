package patch

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func symlinkOrSkip(t *testing.T, target, link string) {
	t.Helper()
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("symbolic link creation is unavailable: %v", err)
	}
}

func TestRootPinSurvivesParentSwap(t *testing.T) {
	env, cwd := editTestEnv(t)
	outside := filepath.Join(t.TempDir(), "outside")
	if err := os.MkdirAll(outside, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(outside, "f.txt"), []byte("before\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	writeFixture(t, cwd, "sub/f.txt", "before\n")
	pin, err := pinRoot(env, "sub/f.txt", filepath.Join(cwd, "sub", "f.txt"))
	if err != nil {
		t.Fatal(err)
	}
	defer pin.close()
	moved := filepath.Join(cwd, "sub-moved")
	if err := os.Rename(filepath.Join(cwd, "sub"), moved); err != nil {
		t.Fatal(err)
	}
	symlinkOrSkip(t, outside, filepath.Join(cwd, "sub"))
	if err := pin.publishOver("f.txt", []byte("after\n"), 0o644); err != nil {
		t.Fatalf("publish through the pin: %v", err)
	}
	requireFile(t, filepath.Join(outside, "f.txt"), "before\n")
	requireFile(t, filepath.Join(moved, "f.txt"), "after\n")
}

func TestRootPinRefusesSymlinkedMissingParent(t *testing.T) {
	env, cwd := editTestEnv(t)
	outside := filepath.Join(t.TempDir(), "outside")
	if err := os.MkdirAll(outside, 0o700); err != nil {
		t.Fatal(err)
	}
	pin, err := pinRoot(env, "nd/g.txt", filepath.Join(cwd, "nd", "g.txt"))
	if err != nil {
		t.Fatal(err)
	}
	defer pin.close()
	symlinkOrSkip(t, outside, filepath.Join(cwd, "nd"))
	err = pin.mkdirChain("nd/g.txt", 0o700)
	if err == nil || !strings.Contains(err.Error(), "symbolic link") {
		t.Fatalf("mkdirChain through a symlinked parent: err = %v, want a symbolic link refusal", err)
	}
	requireMissing(t, filepath.Join(outside, "g.txt"))
}

func TestRootPinRefusesSymlinkLeaf(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "target.txt", "x\n")
	symlinkOrSkip(t, filepath.Join(cwd, "target.txt"), filepath.Join(cwd, "link.txt"))
	pin, err := pinRoot(env, "link.txt", filepath.Join(cwd, "link.txt"))
	pin.close()
	if err == nil || !strings.Contains(err.Error(), "symbolic link") {
		t.Fatalf("pin of a symlink leaf: err = %v, want a symbolic link refusal", err)
	}
}

func TestRootPinSeesLeafThroughGapCreatedParent(t *testing.T) {
	env, cwd := editTestEnv(t)
	pin, err := pinRoot(env, "nd/g.txt", filepath.Join(cwd, "nd", "g.txt"))
	if err != nil {
		t.Fatal(err)
	}
	defer pin.close()
	if err = os.MkdirAll(filepath.Join(cwd, "nd"), 0o700); err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filepath.Join(cwd, "nd", "g.txt"), []byte("appeared\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	exists, err := pin.leafExistsAfterChain("g.txt", "nd/g.txt")
	if err != nil {
		t.Fatal(err)
	}
	if !exists {
		t.Fatal("a leaf created in the gap was not seen through the pin")
	}
}

func TestRootPinReadsAndLinks(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "sub/f.txt", "one\n")
	pin, err := pinRoot(env, "sub/f.txt", filepath.Join(cwd, "sub", "f.txt"))
	if err != nil {
		t.Fatal(err)
	}
	defer pin.close()
	data, err := pin.readFileAt("f.txt")
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "one\n" {
		t.Fatalf("pinned read = %q, want %q", data, "one\n")
	}
	if err = pin.linkFileAt("f.txt", "g.txt"); err != nil {
		t.Fatal(err)
	}
	requireFile(t, filepath.Join(cwd, "sub", "g.txt"), "one\n")
}
