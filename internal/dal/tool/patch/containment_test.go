package patch

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

func TestPatchRejectsSymlinkEscape(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("symlink creation requires additional privileges")
	}
	env, cwd := editTestEnv(t)
	outside := filepath.Join(t.TempDir(), "outside.txt")
	if err := os.WriteFile(outside, []byte("secret\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(outside, filepath.Join(cwd, "link.txt")); err != nil {
		t.Fatal(err)
	}
	_, err := runPatch(t, env, Replace, `{"changes":[{"path":"link.txt","old":"secret","new":"changed"}]}`)
	if err == nil {
		t.Fatal("symlink escape was accepted")
	}
	data, readErr := os.ReadFile(outside)
	if readErr != nil || string(data) != "secret\n" {
		t.Fatalf("outside file changed: %q %v", data, readErr)
	}
}
