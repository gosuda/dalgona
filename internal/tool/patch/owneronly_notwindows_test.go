//go:build !windows

package patch

import (
	"os"
	"path/filepath"
	"testing"
)

func TestUnchangedFileModePreservesOwnerOnly(t *testing.T) {
	if os.Getuid() == 0 {
		t.Skip("mode check is not meaningful as root")
	}
	env, cwd := editTestEnv(t)
	path := filepath.Join(cwd, "f.txt")
	writeFixture(t, cwd, "f.txt", "before\n")
	if err := os.Chmod(path, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := runPatch(t, env, Replace, `{"changes":[{"path":"f.txt","old":"before","new":"after"}]}`); err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode().Perm() != 0o600 {
		t.Fatalf("mode %o", info.Mode().Perm())
	}
}
