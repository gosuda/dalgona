//go:build windows

package runner

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestLocal_survivesReopen(t *testing.T) {
	dir := t.TempDir()
	local, openErr := NewLocal(filepath.Join(dir, "runs"))
	if openErr != nil {
		t.Fatal(openErr)
	}
	startWindowsProcess(t, local, ProcessSpec{ID: "survival", Argv: []string{"cmd", "/C", "ping -n 3 127.0.0.1 >nul & echo done"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
	if closeErr := local.Close(); closeErr != nil {
		t.Fatal(closeErr)
	}
	reopened, reopenErr := NewLocal(filepath.Join(dir, "runs"))
	if reopenErr != nil {
		t.Fatal(reopenErr)
	}
	state := windowsWaitForExit(t, reopened, "survival", 30*time.Second)
	if state.Exit.Code != 0 || state.Alive || !strings.Contains(string(state.Data), "done") {
		t.Fatalf("unexpected recovered state: %+v", state)
	}
}

func TestLocal_rejectsMismatchedProcessIdentity(t *testing.T) {
	dir := t.TempDir()
	local := newWindowsLocal(t, filepath.Join(dir, "runs"))
	startWindowsProcess(t, local, ProcessSpec{ID: "identity", Argv: []string{"cmd", "/C", "ping -n 31 127.0.0.1 >nul"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
	windowsWaitForAlive(t, local, "identity", 30*time.Second)
	path := filepath.Join(dir, "runs", "identity", "pid")
	original, readErr := os.ReadFile(path)
	if readErr != nil {
		t.Fatal(readErr)
	}
	fields := strings.Fields(string(original))
	if len(fields) != 2 || fields[1] == "" {
		t.Fatalf("pid record = %q", original)
	}
	t.Cleanup(func() {
		_ = os.WriteFile(path, original, 0o600)
	})
	if writeErr := os.WriteFile(path, []byte(fields[0]+" 0\n"), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	state, stateErr := local.Read(t.Context(), "identity", 0)
	if stateErr != nil {
		t.Fatal(stateErr)
	}
	if state.Alive {
		t.Fatal("mismatched process start time reported alive")
	}
	if restoreErr := os.WriteFile(path, original, 0o600); restoreErr != nil {
		t.Fatal(restoreErr)
	}
}

func TestLocal_signalPreservesExitRecord(t *testing.T) {
	dir := t.TempDir()
	local := newWindowsLocal(t, filepath.Join(dir, "runs"))
	startWindowsProcess(t, local, ProcessSpec{ID: "signal", Argv: []string{"cmd", "/C", "ping -n 31 127.0.0.1 >nul"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
	windowsWaitForAlive(t, local, "signal", 30*time.Second)
	if signalErr := local.Signal(t.Context(), "signal", SignalTerm); signalErr != nil {
		t.Fatal(signalErr)
	}
	state := windowsWaitForExit(t, local, "signal", 30*time.Second)
	if state.Exit.Code != 1 || state.Exit.Signal != "" {
		t.Fatalf("signaled exit = %+v", state.Exit)
	}
	again, againErr := local.Read(t.Context(), "signal", 0)
	if againErr != nil {
		t.Fatal(againErr)
	}
	if again.Exit == nil || again.Exit.Code != 1 || again.Exit.Signal != "" {
		t.Fatalf("exit record changed = %+v", again.Exit)
	}
}

func newWindowsLocal(t *testing.T, dir string) *Local {
	t.Helper()
	local, openErr := NewLocal(dir)
	if openErr != nil {
		t.Fatal(openErr)
	}
	return local
}

func startWindowsProcess(t *testing.T, local *Local, spec ProcessSpec) {
	t.Helper()
	if startErr := local.Start(t.Context(), spec); startErr != nil {
		t.Fatal(startErr)
	}
	t.Cleanup(func() {
		cleanupWindowsProcess(t, local, spec.ID)
	})
}

func cleanupWindowsProcess(t *testing.T, local *Local, id string) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	for {
		state, readErr := local.Read(ctx, id, 0)
		if readErr == nil && (state.Exit != nil || !state.Alive) {
			return
		}
		signalErr := local.Signal(ctx, id, SignalKill)
		if errors.Is(signalErr, ErrProcessNotFound) {
			return
		}
		if ctx.Err() != nil {
			t.Errorf("cleanup process %s: %v", id, signalErr)
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func windowsWaitForExit(t *testing.T, local *Local, id string, timeout time.Duration) Output {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for {
		state, readErr := local.Read(t.Context(), id, 0)
		if readErr != nil {
			t.Fatal(readErr)
		}
		if state.Exit != nil {
			return state
		}
		if time.Now().After(deadline) {
			t.Fatalf("process %s did not exit", id)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func windowsWaitForAlive(t *testing.T, local *Local, id string, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for {
		state, readErr := local.Read(t.Context(), id, 0)
		if readErr != nil {
			t.Fatal(readErr)
		}
		if state.Alive {
			if _, groupErr := readProcessGroup(filepath.Join(local.dir, id)); groupErr == nil {
				return
			}
		}
		if time.Now().After(deadline) {
			t.Fatalf("process %s did not become alive", id)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
