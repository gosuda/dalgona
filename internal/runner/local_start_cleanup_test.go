//go:build !windows

package runner

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func rerunOnce(t *testing.T, local *Local, dir string, id string, counter string) {
	t.Helper()
	okSpec := ProcessSpec{ID: id, Argv: []string{"sh", "-c", "printf x >> \"$1\"", "_", counter}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}
	retryErr := local.Start(t.Context(), okSpec)
	if retryErr != nil {
		t.Fatal(retryErr)
	}
	state := waitForExit(t, local, id, 5*time.Second)
	if state.Exit == nil || state.Exit.Code != 0 {
		t.Fatalf("second Start exit = %+v", state.Exit)
	}
	data, readErr := os.ReadFile(counter)
	if readErr != nil {
		t.Fatal(readErr)
	}
	if string(data) != "x" {
		t.Fatalf("command ran %q, want %q", string(data), "x")
	}
}

func TestLocal_failedStartCleansRunDirectory(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	id := "cleanup-failure"
	runDir := filepath.Join(dir, "runs", id)
	if err := os.MkdirAll(runDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(runDir, "marker"), []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	removeRunDir(runDir)
	if _, statErr := os.Stat(runDir); !errors.Is(statErr, fs.ErrNotExist) {
		t.Fatalf("run directory remains after failed Start")
	}
	failSpec := ProcessSpec{ID: id, Argv: []string{"sh", "-c", "echo hi"}, Dir: filepath.Join(dir, "missing-workdir"), Sandbox: Sandbox{Mode: SandboxOff}}
	startErr := local.Start(t.Context(), failSpec)
	if startErr == nil {
		t.Fatal("failed Start returned nil")
	}
	if _, statErr := os.Stat(runDir); !errors.Is(statErr, fs.ErrNotExist) {
		t.Fatalf("run directory remains after failed Start")
	}
	counter := filepath.Join(dir, "count")
	rerunOnce(t, local, dir, id, counter)
}
