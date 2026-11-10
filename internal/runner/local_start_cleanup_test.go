//go:build !windows

package runner

import (
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"
)

func hugePadEnv(n int) []string {
	env := make([]string, 0, n)
	for i := range n {
		env = append(env, "DALGONA_TEST_PAD_"+strconv.Itoa(i)+"=x")
	}
	return env
}

func plantMarker(ctx context.Context, runDir string, created chan error) {
	deadline := time.Now().Add(5 * time.Second)
	for {
		select {
		case <-ctx.Done():
			created <- ctx.Err()
			return
		default:
		}
		_, statErr := os.Stat(runDir)
		if statErr == nil {
			break
		}
		if time.Now().After(deadline) {
			created <- errors.New("marker wait timed out")
			return
		}
		time.Sleep(100 * time.Microsecond)
	}
	marker := filepath.Join(runDir, "marker")
	writeErr := os.WriteFile(marker, []byte("x"), 0o600)
	if writeErr != nil {
		created <- writeErr
		return
	}
	created <- nil
}

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
	created := make(chan error, 1)
	go plantMarker(t.Context(), runDir, created)
	failSpec := ProcessSpec{ID: id, Argv: []string{"sh", "-c", "echo hi"}, Dir: filepath.Join(dir, "missing-workdir"), Env: hugePadEnv(20000), Sandbox: Sandbox{Mode: SandboxOff}}
	startErr := local.Start(t.Context(), failSpec)
	var plantedErr error
	select {
	case plantedErr = <-created:
	case <-time.After(6 * time.Second):
		t.Fatal("marker wait timed out")
	}
	if startErr == nil {
		t.Fatal("failed Start returned nil")
	}
	if plantedErr != nil {
		t.Fatal(plantedErr)
	}
	_, statErr := os.Stat(runDir)
	if !errors.Is(statErr, fs.ErrNotExist) {
		t.Fatalf("run directory remains after failed Start")
	}
	counter := filepath.Join(dir, "count")
	rerunOnce(t, local, dir, id, counter)
}
