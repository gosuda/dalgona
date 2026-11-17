//go:build windows

package patch

import (
	"os"
	"path/filepath"
	"testing"
)

func TestReplaceEditsReadOnlyFile(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", "one\ntwo\n")
	path := filepath.Join(cwd, "f.txt")
	if chmodErr := os.Chmod(path, 0o400); chmodErr != nil {
		t.Fatal(chmodErr)
	}
	if _, err := runPatch(t, env, Replace, `{"changes":[{"path":"f.txt","old":"one","new":"ONE"}]}`); err != nil {
		t.Fatalf("patch read-only file: %v", err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "ONE\ntwo\n" {
		t.Fatalf("read-only edit result %q, want %q", actual, "ONE\ntwo\n")
	}
	info, statErr := os.Stat(path)
	if statErr != nil {
		t.Fatal(statErr)
	}
	if info.Mode().Perm()&0o200 != 0 {
		t.Fatalf("read-only file mode %v, want without owner write", info.Mode().Perm())
	}
}
