//go:build !windows

package runner

import (
	"bytes"
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
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
	state := waitForExit(t, reopened, "survival")
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
	waitForExit(t, local, "same")
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
	state := waitForExit(t, local, "env")
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
	state := waitForExit(t, local, "self")
	want := "/proc/self/exe"
	if runtime.GOOS != "linux" {
		executable, err := os.Executable()
		if err != nil {
			t.Fatal(err)
		}
		want = executable
	}
	if strings.TrimSpace(string(state.Data)) != want {
		t.Fatalf("@self output = %q, want %q", state.Data, want)
	}
}

func TestLocal_rejectsMismatchedProcessIdentity(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	gate := startGatedProcess(t, local, ProcessSpec{ID: "identity", Argv: []string{"sleep", "30"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}, "start")
	gate.await(t)
	if !aliveState(t, local, "identity") {
		t.Fatal("process reported dead before its identity was changed")
	}
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
	if aliveState(t, local, "identity") {
		t.Fatal("mismatched process start time reported alive")
	}
	if err := os.WriteFile(path, original, 0o600); err != nil {
		t.Fatal(err)
	}
	gate.release()
}

func aliveState(t *testing.T, local *Local, id string) bool {
	t.Helper()
	state, err := local.Read(t.Context(), id, 0)
	if err != nil {
		t.Fatal(err)
	}
	return state.Alive
}

func TestWriteSpawnPID_rejectsUnavailableStartTime(t *testing.T) {
	runDir := filepath.Join(t.TempDir(), "run")
	if err := os.Mkdir(runDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := writeSpawnPID(runDir, 1<<30); err == nil {
		t.Fatal("writeSpawnPID succeeded with an unavailable process start time")
	}
	if _, err := os.Stat(filepath.Join(runDir, "pid")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("pid file state = %v, want absent", err)
	}
}

func TestLocal_signalPreservesExitRecord(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	gate := startGatedProcess(t, local, ProcessSpec{ID: "signal", Argv: []string{"sleep", "30"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}, "group")
	gate.await(t)
	if err := local.Signal(t.Context(), "signal", SignalTerm); err != nil {
		t.Fatal(err)
	}
	gate.release()
	state := waitForExit(t, local, "signal")
	if state.Exit.Code == 0 || state.Exit.Signal == "" {
		t.Fatalf("signaled exit = %+v", state.Exit)
	}
}

func TestLocal_signalBeforePIDPreventsStart(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	gate := startGatedProcess(t, local, ProcessSpec{ID: "early", Argv: []string{"sh", "-c", "touch started; sleep 30"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}, "pid")
	gate.await(t)
	runDir := filepath.Join(dir, "runs", "early")
	if _, err := os.Stat(filepath.Join(runDir, "pid")); err != nil {
		t.Fatalf("parent did not write pid before the signal: %v", err)
	}
	if err := local.Signal(t.Context(), "early", SignalTerm); err != nil {
		t.Fatal(err)
	}
	gate.release()
	state := waitForExit(t, local, "early")
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

func TestLocal_signalBeforeGroupReturnsNilAndEndsTerminated(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	gate := startGatedProcess(t, local, ProcessSpec{ID: "starting", Argv: []string{"sh", "-c", "sleep 30 & wait"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}, "start")
	gate.await(t)
	runDir := filepath.Join(dir, "runs", "starting")
	if _, err := os.Stat(filepath.Join(runDir, "pgid")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("pgid existed before the signal: %v", err)
	}
	if err := local.Signal(t.Context(), "starting", SignalTerm); err != nil {
		t.Fatal(err)
	}
	gate.release()
	state := waitForExit(t, local, "starting")
	if state.Exit.Code == 0 || state.Exit.Signal != syscall.SIGTERM.String() {
		t.Fatalf("signaled exit = %+v", state.Exit)
	}
	pgid, err := readProcessGroup(runDir)
	if err != nil {
		t.Fatalf("command never started: %v", err)
	}
	if !processGroupStopped(pgid) {
		t.Fatalf("process group %d outlived the signal", pgid)
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
		err := local.Signal(context.Background(), spec.ID, SignalKill)
		if errors.Is(err, ErrProcessNotFound) {
			return
		}
		if err != nil {
			t.Errorf("cleanup process %s: %v", spec.ID, err)
			return
		}
		if err := waitShimExit(local, spec.ID); err != nil {
			t.Errorf("cleanup process %s: %v", spec.ID, err)
		}
	})
}

func TestLocal_sandboxConfinesWrites(t *testing.T) {
	if UnavailableCause() != "" {
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
	state := waitForExit(t, local, "sandbox")
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

func TestLocal_sandboxDeniesWriteThroughSymlink(t *testing.T) {
	if UnavailableCause() != "" {
		t.Skip("landlock ABI is 0")
	}
	fixture := setupSymlinkEscape(t)
	local := newTestLocal(t, filepath.Join(fixture.dir, "runs"))
	script := `touch ./inside-ok; if echo pwned >> "$1"; then echo link-ok; else echo link-denied; fi`
	startTestProcess(t, local, ProcessSpec{ID: "symlink", Argv: []string{"sh", "-c", script, "_", fixture.link}, Dir: fixture.workspace, Env: fixture.env, Sandbox: Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{fixture.workspace}}})
	state := waitForExit(t, local, "symlink")
	if state.Exit.Code != 0 {
		t.Fatalf("sandboxed script failed: %+v output %s", state.Exit, state.Data)
	}
	if !strings.Contains(string(state.Data), "link-denied") {
		t.Fatalf("write through symlink was not denied: %s", state.Data)
	}
	if _, err := os.Stat(filepath.Join(fixture.workspace, "inside-ok")); err != nil {
		t.Fatalf("write inside the roots failed: %v", err)
	}
	data, err := os.ReadFile(fixture.outside)
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != "secret" {
		t.Fatalf("outside file changed through symlink: %q", data)
	}
}

type symlinkFixture struct {
	dir       string
	workspace string
	link      string
	outside   string
	env       []string
}

func setupSymlinkEscape(t *testing.T) symlinkFixture {
	t.Helper()
	dir := t.TempDir()
	workspace := filepath.Join(dir, "workspace")
	tmproot := filepath.Join(dir, "tmp")
	cache := filepath.Join(dir, "cache")
	home := filepath.Join(dir, "home")
	for _, path := range []string{workspace, tmproot, cache, home} {
		if err := os.Mkdir(path, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	outside := filepath.Join(t.TempDir(), "secret")
	if err := os.WriteFile(outside, []byte("secret"), 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(workspace, "escape")
	if err := os.Symlink(outside, link); err != nil {
		t.Fatal(err)
	}
	env := setEnvironment(os.Environ(), "HOME", home)
	env = setEnvironment(env, "TMPDIR", tmproot)
	env = setEnvironment(env, "XDG_CACHE_HOME", cache)
	return symlinkFixture{dir: dir, workspace: workspace, link: link, outside: outside, env: env}
}

func waitShimExit(local *Local, id string) error {
	identity, err := readIdentity(filepath.Join(local.dir, id, "pid"))
	if err != nil {
		return err
	}
	return waitProcessExit(identity.pid, identity.start)
}

func waitForExit(t *testing.T, local *Local, id string) Output {
	t.Helper()
	if err := waitShimExit(local, id); err != nil {
		t.Fatalf("wait for shim %s: %v", id, err)
	}
	state, err := local.Read(t.Context(), id, 0)
	if err != nil {
		t.Fatal(err)
	}
	if state.Exit == nil {
		t.Fatalf("shim %s exited without an exit record: %+v", id, state)
	}
	return state
}

func TestLocal_readDrainsTailAfterExit(t *testing.T) {
	local := newTestLocal(t, filepath.Join(t.TempDir(), "runs"))
	runDir := filepath.Join(local.dir, "tail")
	if err := os.MkdirAll(runDir, 0o700); err != nil {
		t.Fatal(err)
	}
	outPath := filepath.Join(runDir, "out")
	if err := os.WriteFile(outPath, []byte("abc"), 0o600); err != nil {
		t.Fatal(err)
	}
	local.afterSnapshot = func() {
		if err := os.WriteFile(outPath, []byte("abcdef"), 0o600); err != nil {
			t.Error(err)
			return
		}
		if err := writeExit(filepath.Join(runDir, "exit"), Exit{Code: 0, EndedAt: time.Now()}); err != nil {
			t.Error(err)
		}
	}
	t.Cleanup(func() { local.afterSnapshot = nil })
	state, err := local.Read(t.Context(), "tail", 0)
	if err != nil {
		t.Fatal(err)
	}
	if string(state.Data) != "abcdef" || state.Exit == nil || state.Exit.Code != 0 {
		t.Fatalf("terminal read = %q exit %+v, want abcdef with exit 0", state.Data, state.Exit)
	}
}

func TestLocal_readDrainsMultiPageTerminal(t *testing.T) {
	local := newTestLocal(t, filepath.Join(t.TempDir(), "runs"))
	runDir := filepath.Join(local.dir, "big")
	if err := os.MkdirAll(runDir, 0o700); err != nil {
		t.Fatal(err)
	}
	want := bytes.Repeat([]byte("7"), 2*readPageSize+13)
	if err := os.WriteFile(filepath.Join(runDir, "out"), want, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := writeExit(filepath.Join(runDir, "exit"), Exit{Code: 0, EndedAt: time.Now()}); err != nil {
		t.Fatal(err)
	}
	state, err := local.Read(t.Context(), "big", 0)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(state.Data, want) || state.Exit == nil {
		t.Fatalf("terminal read = %d bytes exit %+v, want %d with exit", len(state.Data), state.Exit, len(want))
	}
}
