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

var errTestSpawnFailure = errors.New("runner: test spawn failure")

func TestLocal_spawnFailureKillsCommand(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	id := "spawn-failure"
	runDir := filepath.Join(dir, "runs", id)
	started := filepath.Join(dir, "started")
	var target int
	oldWriteSpawnPID := writeSpawnPIDFunc
	writeSpawnPIDFunc = func(d string, _ int) error {
		waitForFile(t, filepath.Join(d, "pgid"), 10*time.Second)
		waitForFile(t, started, 10*time.Second)
		group, err := readProcessGroup(d)
		if err != nil {
			t.Fatal(err)
		}
		target = group
		return errTestSpawnFailure
	}
	defer func() { writeSpawnPIDFunc = oldWriteSpawnPID }()
	t.Cleanup(func() {
		if target > 0 {
			_ = signalProcess(0, target, SignalKill)
		}
	})
	spec := ProcessSpec{ID: id, Argv: []string{"sh", "-c", `touch "$1"; sleep 30`, "_", started}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}
	if err := local.Start(t.Context(), spec); !errors.Is(err, errTestSpawnFailure) {
		t.Fatalf("Start error = %v, want test spawn failure", err)
	}
	deadline := time.Now().Add(10 * time.Second)
	for !processGroupStopped(target) {
		if time.Now().After(deadline) {
			t.Fatalf("process group %d outlived the failed start", target)
		}
		time.Sleep(10 * time.Millisecond)
	}
	if _, err := os.Stat(runDir); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("run directory remains after failed Start")
	}
	writeSpawnPIDFunc = oldWriteSpawnPID
	counter := filepath.Join(dir, "count")
	rerunOnce(t, local, dir, id, counter)
}

func TestSpawnNeverStarted(t *testing.T) {
	dir := t.TempDir()
	makeRunDir := func(name string) string {
		t.Helper()
		path := filepath.Join(dir, name)
		if err := os.Mkdir(path, 0o700); err != nil {
			t.Fatal(err)
		}
		return path
	}
	checkSpawnNeverStarted(t, makeRunDir("empty"), true)
	withPid := makeRunDir("pid")
	writeRunFile(t, filepath.Join(withPid, "pid"), "123 456\n")
	checkSpawnNeverStarted(t, withPid, false)
	withExit := makeRunDir("exit")
	writeRunExit(t, withExit)
	checkSpawnNeverStarted(t, withExit, true)
	withBoth := makeRunDir("both")
	writeRunFile(t, filepath.Join(withBoth, "pid"), "123 456\n")
	writeRunExit(t, withBoth)
	checkSpawnNeverStarted(t, withBoth, true)
}

func checkSpawnNeverStarted(t *testing.T, runDir string, want bool) {
	t.Helper()
	if got := spawnNeverStarted(runDir); got != want {
		t.Fatalf("spawnNeverStarted = %v, want %v", got, want)
	}
}

func writeRunFile(t *testing.T, path string, data string) {
	t.Helper()
	if err := os.WriteFile(path, []byte(data), 0o600); err != nil {
		t.Fatal(err)
	}
}

func writeRunExit(t *testing.T, runDir string) {
	t.Helper()
	if err := writeExit(filepath.Join(runDir, "exit"), Exit{Code: 1, EndedAt: time.Now()}); err != nil {
		t.Fatal(err)
	}
}
