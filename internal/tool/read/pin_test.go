package read

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func plantSymlink(t *testing.T, link, target string) {
	t.Helper()
	if err := os.Symlink(target, link); err != nil {
		t.Skipf("symlink creation requires additional privileges: %v", err)
	}
}

func TestReadSwapAfterCheckRefused(t *testing.T) {
	env, cwd := readTestEnv(t)
	secret := filepath.Join(t.TempDir(), "secret.txt")
	writeReadFixture(t, secret, "top-secret\n")
	target := filepath.Join(cwd, "swap.txt")
	writeReadFixture(t, target, "benign\n")
	old := testHookReadSwap
	testHookReadSwap = func() {
		_ = os.Remove(target)
		plantSymlink(t, target, secret)
	}
	defer func() { testHookReadSwap = old }()
	result, err := callRead(t, env, `{"path":"swap.txt"}`)
	if err == nil || !strings.Contains(err.Error(), "symlink") {
		t.Fatalf("swap-after-check read error = %v, want symlink refusal", err)
	}
	if strings.Contains(string(result.Output), "top-secret") {
		t.Fatalf("swap-after-check read leaked outside content")
	}
}

func TestReadSpillSymlinkRefused(t *testing.T) {
	env, _ := readTestEnv(t)
	secret := filepath.Join(t.TempDir(), "secret.txt")
	writeReadFixture(t, secret, "top-secret\n")
	spill := filepath.Join(env.Home, "runs", "call", "out")
	if err := os.MkdirAll(filepath.Dir(spill), 0o750); err != nil {
		t.Fatal(err)
	}
	writeReadFixture(t, spill, "spill\n")
	spillArgs := `{"path":` + snapshot.JSONString(spill) + `}`
	if _, err := callRead(t, env, spillArgs); err != nil {
		t.Fatalf("spill read failed: %v", err)
	}
	if err := os.Remove(spill); err != nil {
		t.Fatal(err)
	}
	plantSymlink(t, spill, secret)
	result, err := callRead(t, env, spillArgs)
	if err == nil || !strings.Contains(err.Error(), "symlink") {
		t.Fatalf("spill symlink read error = %v, want symlink refusal", err)
	}
	if strings.Contains(string(result.Output), "top-secret") {
		t.Fatalf("spill symlink read leaked outside content")
	}
}

func TestReadPinnedNormalReads(t *testing.T) {
	env, cwd := readTestEnv(t)
	writeReadFixture(t, filepath.Join(cwd, "ok.txt"), "fine\n")
	if _, err := callRead(t, env, `{"path":"ok.txt"}`); err != nil {
		t.Fatalf("workspace read failed: %v", err)
	}
	spill := filepath.Join(env.Home, "blobs", strings.Repeat("b", 64))
	if err := os.MkdirAll(filepath.Dir(spill), 0o750); err != nil {
		t.Fatal(err)
	}
	writeReadFixture(t, spill, "spill\n")
	result, err := callRead(t, env, `{"path":`+snapshot.JSONString(spill)+`}`)
	if err != nil {
		t.Fatalf("spill read failed: %v", err)
	}
	if !strings.Contains(string(result.Output), "1:spill\n") {
		t.Fatalf("spill read output = %q, want spill content", result.Output)
	}
}

func TestReadPinnedDirListsFromDescriptor(t *testing.T) {
	env, cwd := readTestEnv(t)
	sub := filepath.Join(cwd, "sub")
	if err := os.MkdirAll(sub, 0o755); err != nil {
		t.Fatal(err)
	}
	writeReadFixture(t, filepath.Join(sub, "inner.txt"), "in\n")
	result, err := callRead(t, env, `{"path":"sub"}`)
	if err != nil {
		t.Fatalf("dir read failed: %v", err)
	}
	if !strings.Contains(string(result.Output), "inner.txt\n") {
		t.Fatalf("dir read output = %q, want entry", result.Output)
	}
	outside := t.TempDir()
	writeReadFixture(t, filepath.Join(outside, "secret.txt"), "top-secret\n")
	if removeErr := os.RemoveAll(sub); removeErr != nil {
		t.Fatal(removeErr)
	}
	plantSymlink(t, sub, outside)
	result, err = callRead(t, env, `{"path":"sub"}`)
	if err == nil || !strings.Contains(err.Error(), "symlink") {
		t.Fatalf("swapped dir read error = %v, want symlink refusal", err)
	}
	if strings.Contains(string(result.Output), "secret") {
		t.Fatalf("swapped dir read leaked outside entries")
	}
}
