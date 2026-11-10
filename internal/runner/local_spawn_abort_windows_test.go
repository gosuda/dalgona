//go:build windows

package runner

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"testing"
	"time"
)

var errTestSpawnFailureWindows = errors.New("runner: test spawn failure")

func TestLocal_spawnFailureKillsCommand(t *testing.T) {
	dir := t.TempDir()
	local := newWindowsLocal(t, filepath.Join(dir, "runs"))
	id := "spawn-failure"
	runDir := filepath.Join(dir, "runs", id)
	started := filepath.Join(dir, "started")
	var target int
	oldWriteSpawnPID := writeSpawnPIDFunc
	writeSpawnPIDFunc = func(d string, _ int) error {
		windowsWaitForRunFile(t, filepath.Join(d, "pgid"), 30*time.Second)
		windowsWaitForRunFile(t, started, 30*time.Second)
		group, err := readProcessGroup(d)
		if err != nil {
			t.Fatal(err)
		}
		target = group
		return errTestSpawnFailureWindows
	}
	defer func() { writeSpawnPIDFunc = oldWriteSpawnPID }()
	t.Cleanup(func() {
		if target > 0 {
			_ = terminateSpawnPID(target)
		}
	})
	spec := ProcessSpec{ID: id, Argv: []string{"cmd", "/C", "echo started > started & ping -n 60 127.0.0.1 >nul"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}
	if err := local.Start(t.Context(), spec); !errors.Is(err, errTestSpawnFailureWindows) {
		t.Fatalf("Start error = %v, want test spawn failure", err)
	}
	deadline := time.Now().Add(30 * time.Second)
	for processIsAlive(target) {
		if time.Now().After(deadline) {
			t.Fatalf("process %d outlived the failed start", target)
		}
		time.Sleep(50 * time.Millisecond)
	}
	if _, err := os.Stat(runDir); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("run directory remains after failed Start")
	}
	writeSpawnPIDFunc = oldWriteSpawnPID
	retry := ProcessSpec{ID: id, Argv: []string{"cmd", "/C", "echo ok"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}
	startWindowsProcess(t, local, retry)
	state := windowsWaitForExit(t, local, id, 30*time.Second)
	if state.Exit.Code != 0 {
		t.Fatalf("retry exit = %+v, want code 0", state.Exit)
	}
}

func windowsWaitForRunFile(t *testing.T, path string, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for {
		if _, err := os.Stat(path); err == nil {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("%s did not appear", path)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
