//go:build !windows

package runner

import (
	"bytes"
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"
)

func TestLocal_survivesReopen(t *testing.T) {
	dir := t.TempDir()
	local, err := NewLocal(filepath.Join(dir, "runs"))
	if err != nil {
		t.Fatal(err)
	}
	startTestProcess(t, local, ProcessSpec{ID: "survival", Argv: []string{"sh", "-c", "sleep 2; echo done"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
	if closeErr := local.Close(); closeErr != nil {
		t.Fatal(closeErr)
	}
	reopened, err := NewLocal(filepath.Join(dir, "runs"))
	if err != nil {
		t.Fatal(err)
	}
	state := waitForExit(t, reopened, "survival", 8*time.Second)
	if state.Exit.Code != 0 || state.Alive || !strings.Contains(string(state.Data), "done") {
		t.Fatalf("unexpected recovered state: %+v", state)
	}
}

func TestLocal_startIsIdempotent(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	counter := filepath.Join(dir, "count")
	spec := ProcessSpec{ID: "same", Argv: []string{"sh", "-c", `printf x >> "$1"`, "_", counter}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}
	startTestProcess(t, local, spec)
	if err := local.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	waitForExit(t, local, "same", 5*time.Second)
	data, err := os.ReadFile(counter)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(data, []byte("x")) {
		t.Fatalf("command ran %d times", len(data))
	}
}

func TestLimitedWriter_cap(t *testing.T) {
	previous := outputLimit
	outputLimit = 64
	t.Cleanup(func() { outputLimit = previous })
	var out bytes.Buffer
	writer := &limitedWriter{w: &out, limit: outputLimit}
	chunk := bytes.Repeat([]byte("0123456789abcdef"), 4)
	for range 128 {
		n, err := writer.Write(chunk)
		if err != nil {
			t.Fatal(err)
		}
		if n != len(chunk) {
			t.Fatalf("Write returned %d, want %d", n, len(chunk))
		}
	}
	data := out.Bytes()
	notice := "\n[dalgona: output beyond 1 GiB discarded]\n"
	if len(data) != 64+len(notice) {
		t.Fatalf("output length = %d, want %d", len(data), 64+len(notice))
	}
	if !bytes.Equal(data[:64], bytes.Repeat([]byte("0123456789abcdef"), 4)) {
		t.Fatal("output prefix differs from command output")
	}
	if bytes.Count(data, []byte("[dalgona: output beyond 1 GiB discarded]")) != 1 {
		t.Fatal("cap notice count differs from one")
	}
}

func TestLimitedWriter_exactCap(t *testing.T) {
	var out bytes.Buffer
	writer := &limitedWriter{w: &out, limit: 64}
	chunk := bytes.Repeat([]byte("x"), 64)
	if _, err := writer.Write(chunk); err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(out.Bytes(), chunk) {
		t.Fatalf("exact cap output changed: %q", out.Bytes())
	}
}

func TestLocal_environmentDefaults(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	env := []string{"PATH=" + os.Getenv("PATH"), "CI=custom", "PAGER=less"}
	startTestProcess(t, local, ProcessSpec{ID: "env", Argv: []string{"sh", "-c", "env"}, Dir: dir, Env: env, Sandbox: Sandbox{Mode: SandboxOff}})
	state := waitForExit(t, local, "env", 5*time.Second)
	values := map[string]string{}
	for line := range strings.SplitSeq(string(state.Data), "\n") {
		key, value, ok := strings.Cut(line, "=")
		if ok {
			values[key] = value
		}
	}
	want := map[string]string{
		"CI": "custom", "DEBIAN_FRONTEND": "noninteractive", "GIT_TERMINAL_PROMPT": "0",
		"GIT_EDITOR": ":", "GIT_SEQUENCE_EDITOR": ":", "GIT_MERGE_AUTOEDIT": "no",
		"GIT_PAGER": "cat", "PAGER": "less", "EDITOR": ":", "npm_config_yes": "true", "PIP_NO_INPUT": "1",
	}
	for key, value := range want {
		if values[key] != value {
			t.Errorf("%s = %q, want %q", key, values[key], value)
		}
	}
}

func TestLocal_resolvesSelf(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	startTestProcess(t, local, ProcessSpec{ID: "self", Argv: []string{"@self", "__self-test"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
	state := waitForExit(t, local, "self", 5*time.Second)
	executable, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	if strings.TrimSpace(string(state.Data)) != executable {
		t.Fatalf("@self output = %q, want %q", state.Data, executable)
	}
}

func TestLocal_rejectsMismatchedProcessIdentity(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	startTestProcess(t, local, ProcessSpec{ID: "identity", Argv: []string{"sleep", "30"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
	waitForAlive(t, local, "identity", 5*time.Second)
	path := filepath.Join(dir, "runs", "identity", "pid")
	original, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	fields := strings.Fields(string(original))
	if len(fields) != 2 || fields[1] == "" {
		t.Fatalf("pid record = %q", original)
	}
	if writeErr := os.WriteFile(path, []byte(fields[0]+" 0\n"), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	state, err := local.Read(t.Context(), "identity", 0)
	if err != nil {
		t.Fatal(err)
	}
	if state.Alive {
		t.Fatal("mismatched process start time reported alive")
	}
	if err := os.WriteFile(path, original, 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestLocal_signalPreservesExitRecord(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	startTestProcess(t, local, ProcessSpec{ID: "signal", Argv: []string{"sleep", "30"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}})
	waitForAlive(t, local, "signal", 5*time.Second)
	if err := local.Signal(t.Context(), "signal", SignalTerm); err != nil {
		t.Fatal(err)
	}
	state := waitForExit(t, local, "signal", 5*time.Second)
	if state.Exit.Code == 0 || state.Exit.Signal == "" {
		t.Fatalf("signaled exit = %+v", state.Exit)
	}
}

func TestLocal_signalBeforePIDPreventsStart(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	env := append(os.Environ(), shimPauseEnvironment+"=pid:300ms")
	startTestProcess(t, local, ProcessSpec{ID: "early", Argv: []string{"sh", "-c", "touch started; sleep 30"}, Dir: dir, Env: env, Sandbox: Sandbox{Mode: SandboxOff}})
	runDir := filepath.Join(dir, "runs", "early")
	if _, err := os.Stat(filepath.Join(runDir, "pid")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("pid existed before the signal: %v", err)
	}
	if err := local.Signal(t.Context(), "early", SignalTerm); err != nil {
		t.Fatal(err)
	}
	state := waitForExit(t, local, "early", 5*time.Second)
	if state.Exit.Code == 0 || state.Exit.Signal != syscall.SIGTERM.String() {
		t.Fatalf("canceled exit = %+v", state.Exit)
	}
	if _, err := os.Stat(filepath.Join(runDir, "pgid")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("command group was created after the signal: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "started")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("command ran after the signal: %v", err)
	}
}

func TestLocal_signalBeforeProcessGroupStopsCommand(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	env := append(os.Environ(), shimPauseEnvironment+"=start:400ms")
	startTestProcess(t, local, ProcessSpec{ID: "starting", Argv: []string{"sh", "-c", "sleep 30 & wait"}, Dir: dir, Env: env, Sandbox: Sandbox{Mode: SandboxOff}})
	runDir := filepath.Join(dir, "runs", "starting")
	waitForFile(t, filepath.Join(runDir, "pid"), 5*time.Second)
	time.Sleep(100 * time.Millisecond)
	if _, err := os.Stat(filepath.Join(runDir, "pgid")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("pgid existed before the signal: %v", err)
	}
	if err := local.Signal(t.Context(), "starting", SignalTerm); err != nil {
		t.Fatal(err)
	}
	state := waitForExit(t, local, "starting", 30*time.Second)
	if state.Exit.Code == 0 || state.Exit.Signal != syscall.SIGTERM.String() {
		t.Fatalf("signaled exit = %+v", state.Exit)
	}
	pgid, err := readProcessGroup(runDir)
	if err != nil {
		t.Fatalf("command never started: %v", err)
	}
	if err := syscall.Kill(-pgid, 0); !errors.Is(err, syscall.ESRCH) {
		t.Fatalf("process group %d outlived the signal: %v", pgid, err)
	}
}

func waitForFile(t *testing.T, path string, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for {
		if _, err := os.Stat(path); err == nil {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("%s did not appear", path)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func newTestLocal(t *testing.T, dir string) *Local {
	t.Helper()
	local, err := NewLocal(dir)
	if err != nil {
		t.Fatal(err)
	}
	return local
}

func startTestProcess(t *testing.T, local *Local, spec ProcessSpec) {
	t.Helper()
	if err := local.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		defer cancel()
		for {
			state, err := local.Read(ctx, spec.ID, 0)
			if err == nil && (state.Exit != nil || !state.Alive) {
				return
			}
			err = local.Signal(ctx, spec.ID, SignalKill)
			if errors.Is(err, ErrProcessNotFound) {
				return
			}
			if ctx.Err() != nil {
				t.Errorf("cleanup process %s: %v", spec.ID, err)
				return
			}
			time.Sleep(10 * time.Millisecond)
		}
	})
}

func TestLocal_sandboxConfinesWrites(t *testing.T) {
	if !Available() {
		t.Skip("landlock ABI is 0")
	}
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatal(err)
	}
	outside := filepath.Join(home, "outside-"+strconv.Itoa(os.Getpid()))
	t.Cleanup(func() { _ = os.Remove(outside) })
	script := `touch ./inside && touch "$1"; rc=$?; echo rc=$rc`
	startTestProcess(t, local, ProcessSpec{ID: "sandbox", Argv: []string{"sh", "-c", script, "_", outside}, Dir: dir, Sandbox: Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{dir}}})
	state := waitForExit(t, local, "sandbox", 8*time.Second)
	if state.Exit.Code != 0 {
		t.Fatalf("sandboxed script failed: %+v output %s", state.Exit, state.Data)
	}
	if !strings.Contains(string(state.Data), "rc=1") {
		t.Fatalf("write outside the roots was not denied: %s", state.Data)
	}
	if _, err := os.Stat(filepath.Join(dir, "inside")); err != nil {
		t.Fatalf("write inside the roots failed: %v", err)
	}
	if _, err := os.Stat(outside); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("file appeared outside the sandbox: %v", err)
	}
}

func waitForExit(t *testing.T, local *Local, id string, timeout time.Duration) Output {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for {
		state, err := local.Read(t.Context(), id, 0)
		if err != nil {
			t.Fatal(err)
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

func waitForAlive(t *testing.T, local *Local, id string, timeout time.Duration) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for {
		state, err := local.Read(t.Context(), id, 0)
		if err != nil {
			t.Fatal(err)
		}
		if state.Alive {
			if _, err := readProcessGroup(filepath.Join(local.dir, id)); err == nil {
				return
			}
		}
		if time.Now().After(deadline) {
			t.Fatalf("process %s did not become alive", id)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
