package read

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestReadRejectsSymlinkEscape(t *testing.T) {
	env, cwd := readTestEnv(t)
	outsideDir := t.TempDir()
	secret := filepath.Join(outsideDir, "secret.txt")
	writeReadFixture(t, secret, "top-secret\n")
	link := filepath.Join(cwd, "link.txt")
	if err := os.Symlink(secret, link); err != nil {
		t.Skipf("symlink creation requires additional privileges: %v", err)
	}
	if _, err := callRead(t, env, `{"path":"link.txt"}`); err == nil || !strings.Contains(err.Error(), "symlink") {
		t.Fatalf("symlink escape error = %v, want symlink rejection", err)
	}
	if _, err := callRead(t, env, `{"path":"link.txt"}`); err == nil {
		t.Fatalf("symlink escape was accepted")
	}
	writeReadFixture(t, filepath.Join(cwd, "ok.txt"), "fine\n")
	if _, err := callRead(t, env, `{"path":"ok.txt"}`); err != nil {
		t.Fatalf("direct read failed: %v", err)
	}
}

func TestReadRejectsSymlinkedParentEscape(t *testing.T) {
	env, cwd := readTestEnv(t)
	outsideDir := t.TempDir()
	secret := filepath.Join(outsideDir, "secret.txt")
	writeReadFixture(t, secret, "top-secret\n")
	if err := os.Symlink(outsideDir, filepath.Join(cwd, "evil")); err != nil {
		t.Skipf("symlink creation requires additional privileges: %v", err)
	}
	if _, err := callRead(t, env, `{"path":"evil/secret.txt"}`); err == nil || !strings.Contains(err.Error(), "symlink") {
		t.Fatalf("symlinked parent error = %v, want symlink rejection", err)
	}
}
