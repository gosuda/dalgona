package exec

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io/fs"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

type fakeMasker struct{}

func (fakeMasker) Mask(text string) string { return text }

func (fakeMasker) Unmask(text string) string {
	return strings.ReplaceAll(text, "{{secret:CMD}}", "echo unmasked-secret-value")
}

func (fakeMasker) UnmaskStrict(text string) (string, error) {
	return fakeMasker{}.Unmask(text), nil
}

type execOutcome struct {
	result tool.Result
	runErr error
}

func testEnv(t *testing.T) (tool.Env, func()) {
	t.Helper()
	runs, err := runner.NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	cwd := t.TempDir()
	env := tool.Env{Runner: runs, Cwd: cwd, Sandbox: runner.Sandbox{Mode: runner.SandboxWorkspaceWrite, WritableRoots: []string{cwd}}}
	return env, func() { _ = runs.Close() }
}

func printCwdCommand(t *testing.T) string {
	t.Helper()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("PATH", filepath.Dir(exe)+string(os.PathListSeparator)+os.Getenv("PATH"))
	return filepath.Base(exe) + " __print-cwd"
}

func requireCwdOutput(t *testing.T, result tool.Result, dir string) {
	t.Helper()
	want, err := filepath.EvalSymlinks(dir)
	if err != nil {
		t.Fatal(err)
	}
	got, err := filepath.EvalSymlinks(strings.TrimSpace(string(result.Output)))
	if err != nil {
		t.Fatalf("cwd output = %q: %v", result.Output, err)
	}
	if got != want {
		t.Fatalf("cwd = %q, want %q", got, want)
	}
}

func TestExecRunsCommand(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-1", Args: jsontext.Value(`{"command":"printf hello"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK || string(result.Output) != "hello" || result.ExitCode == nil || *result.ExitCode != 0 {
		t.Fatalf("result ok=%v output=%q exit=%v", result.OK, result.Output, result.ExitCode)
	}
}

func TestExecUnmasksCommand(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	env.Masker = fakeMasker{}
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-2", Args: jsontext.Value(`{"command":"{{secret:CMD}}"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK || !strings.Contains(string(result.Output), "unmasked-secret-value") {
		t.Fatalf("unmasked run ok=%v output=%q", result.OK, result.Output)
	}
}

func waitThenPrintLateCommand() string {
	if runtime.GOOS == "windows" {
		return "echo early & ping -n 31 127.0.0.1 >nul & echo late"
	}
	return "echo early; sleep 30; echo late"
}

func createMarkerThenWaitCommand() string {
	if runtime.GOOS == "windows" {
		return "type nul > started & ping -n 31 127.0.0.1 >nul"
	}
	return "touch started; sleep 30"
}

func TestExecTimeoutTerminates(t *testing.T) {
	runsDir := filepath.Join(t.TempDir(), "runs")
	runs, newErr := runner.NewLocal(runsDir)
	if newErr != nil {
		t.Fatal(newErr)
	}
	defer func() {
		_ = runs.Close()
	}()
	cwd := t.TempDir()
	env := tool.Env{Runner: runs, Cwd: cwd, Sandbox: runner.Sandbox{Mode: runner.SandboxWorkspaceWrite, WritableRoots: []string{cwd}}}
	toolExec := New()
	toolExec.grace = 300 * time.Millisecond
	toolExec.killWait = 30 * time.Second
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-3", Args: jsontext.Value(`{"command":` + strconv.Quote(waitThenPrintLateCommand()) + `,"timeout_ms":2000}`)}, env)
	if err == nil {
		t.Fatal("timeout run returned no error")
	}
	if !strings.Contains(err.Error(), "timed out") {
		t.Fatalf("error = %v", err)
	}
	if result.OK {
		t.Fatal("timed out call reported success")
	}
	if result.ExitCode == nil {
		runDir := filepath.Join(runsDir, "run-3")
		t.Fatalf("timed out call settled without an exit record; run directory %s holds: %s", runDir, runDirListing(runDir))
	}
	if !strings.Contains(string(result.Output), "early") {
		t.Fatalf("output before the timeout is missing: %q", result.Output)
	}
	if strings.Contains(string(result.Output), "late") {
		t.Fatalf("output survived the timeout: %q", result.Output)
	}
}

func runDirListing(dir string) string {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return "unreadable: " + err.Error()
	}
	names := make([]string, 0, len(entries))
	for _, entry := range entries {
		names = append(names, entry.Name())
	}
	return strings.Join(names, ", ")
}

func TestExecInterruptBeforeShimPIDSettlesWithExit(t *testing.T) {
	gate := listenShimGate(t)
	t.Setenv("DALGONA_SHIM_TEST_PAUSE", "pid:"+gate.path)
	runsDir := filepath.Join(t.TempDir(), "runs")
	runs, newErr := runner.NewLocal(runsDir)
	if newErr != nil {
		t.Fatal(newErr)
	}
	defer func() {
		_ = runs.Close()
	}()
	cwd := t.TempDir()
	env := tool.Env{Runner: runs, Cwd: cwd, Sandbox: runner.Sandbox{Mode: runner.SandboxWorkspaceWrite, WritableRoots: []string{cwd}}}
	toolExec := New()
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	finished := make(chan execOutcome, 1)
	go func() {
		result, runErr := toolExec.Run(ctx, tool.Call{ID: "run-early", Args: jsontext.Value(`{"command":` + strconv.Quote(createMarkerThenWaitCommand()) + `,"escalate":true}`)}, env)
		finished <- execOutcome{result: result, runErr: runErr}
	}()
	runDir := filepath.Join(runsDir, "run-early")
	parked := gate.awaitShim(t)
	cancel()
	waitForPath(t, filepath.Join(runDir, "cancel"), cancel)
	_ = parked.Close()
	settled := awaitExecOutcome(t, finished)
	if settled.runErr == nil || settled.result.OK {
		t.Fatalf("interrupted run ok=%v err=%v", settled.result.OK, settled.runErr)
	}
	if settled.result.ExitCode == nil || *settled.result.ExitCode == 0 {
		t.Fatalf("interrupted run settled without an exit record: exit=%v err=%v", settled.result.ExitCode, settled.runErr)
	}
	if _, statErr := os.Stat(filepath.Join(cwd, "started")); !errors.Is(statErr, fs.ErrNotExist) {
		t.Fatalf("command ran after the interrupt: %v", statErr)
	}
	if _, statErr := os.Stat(filepath.Join(runDir, "pgid")); !errors.Is(statErr, fs.ErrNotExist) {
		t.Fatalf("command group was created after the interrupt: %v", statErr)
	}
}

type shimGate struct {
	path     string
	listener *net.UnixListener
}

func listenShimGate(t *testing.T) shimGate {
	t.Helper()
	dir, err := os.MkdirTemp("", "dgx")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.RemoveAll(dir) })
	path := filepath.Join(dir, "s")
	listener, err := net.ListenUnix("unix", &net.UnixAddr{Name: path, Net: "unix"})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	return shimGate{path: path, listener: listener}
}

func (g shimGate) awaitShim(t *testing.T) net.Conn {
	t.Helper()
	if err := g.listener.SetDeadline(time.Now().Add(15 * time.Second)); err != nil {
		t.Fatal(err)
	}
	conn, err := g.listener.Accept()
	if err != nil {
		t.Fatalf("shim never reached the pid pause: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

func waitForPath(t *testing.T, path string, cancel context.CancelFunc) {
	t.Helper()
	appear := time.Now().Add(10 * time.Second)
	for {
		if _, statErr := os.Stat(path); statErr == nil {
			return
		}
		if time.Now().After(appear) {
			cancel()
			t.Fatalf("%s did not appear", path)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func awaitExecOutcome(t *testing.T, finished chan execOutcome) execOutcome {
	t.Helper()
	select {
	case settled := <-finished:
		return settled
	case <-time.After(15 * time.Second):
		t.Fatal("interrupted run did not settle")
		return execOutcome{}
	}
}

func TestExecMissingCommandFails(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-4", Args: jsontext.Value(`{"command":"dalgona-definitely-missing-binary"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if result.OK || result.ExitCode == nil || *result.ExitCode == 0 {
		t.Fatalf("missing binary result ok=%v exit=%v", result.OK, result.ExitCode)
	}
}

func TestExecUsesWorkspaceDirectory(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	command := printCwdCommand(t)
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-5", Args: jsontext.Value(`{"command":` + strconv.Quote(command) + `}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	requireCwdOutput(t, result, env.Cwd)
}

func TestExecRejectsBadArguments(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	if _, err := toolExec.Run(t.Context(), tool.Call{ID: "run-6", Args: jsontext.Value(`{"cwd":"."}`)}, env); err == nil {
		t.Fatal("missing command accepted")
	}
	if _, err := toolExec.Run(t.Context(), tool.Call{ID: "run-7", Args: jsontext.Value(`{"command":"true","timeout_ms":0}`)}, env); err == nil {
		t.Fatal("zero timeout accepted")
	}
}

func TestExecKeepsTailOfLongOutput(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-8", Args: jsontext.Value(`{"command":"seq 1 100000"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.Output) > 64*1024+300 {
		t.Fatalf("output length = %d", len(result.Output))
	}
	if !strings.Contains(string(result.Output), "\n100000") {
		t.Fatal("tail lost the last line")
	}
	if !strings.Contains(string(result.Output), "output truncated") {
		t.Fatal("truncation notice missing")
	}
}

func TestExecEscalateRunsUnsandboxed(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	toolExec := New()
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-9", Args: jsontext.Value(`{"command":"true","escalate":true,"justification":"retry after denial"}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	if !result.OK {
		t.Fatalf("escalated run failed: %+v", result)
	}
}

func TestExecRequiresRunner(t *testing.T) {
	toolExec := New()
	if _, err := toolExec.Run(context.Background(), tool.Call{ID: "run-10", Args: jsontext.Value(`{"command":"true"}`)}, tool.Env{}); err == nil {
		t.Fatal("missing runner accepted")
	}
}

func TestExecResolvesRelativeCwdAgainstWorkspace(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	sub := filepath.Join(env.Cwd, "web")
	if err := os.Mkdir(sub, 0o755); err != nil {
		t.Fatal(err)
	}
	toolExec := New()
	command := printCwdCommand(t)
	args := jsontext.Value(`{"command":` + strconv.Quote(command) + `,"cwd":"web"}`)
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-cwd-rel", Args: args}, env)
	if err != nil {
		t.Fatal(err)
	}
	requireCwdOutput(t, result, sub)
}

func TestExecRefusesCwdOutsideWritableRoots(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	outside := t.TempDir()
	toolExec := New()
	args := jsontext.Value(`{"command":"pwd","cwd":` + strconv.Quote(outside) + `}`)
	if _, err := toolExec.Run(t.Context(), tool.Call{ID: "run-cwd-out", Args: args}, env); err == nil {
		t.Fatal("cwd outside the writable roots was accepted")
	}
}

func TestExecUnsandboxedRunsOutsideWritableRoots(t *testing.T) {
	env, closeRunner := testEnv(t)
	defer closeRunner()
	outside := t.TempDir()
	toolExec := New()
	command := printCwdCommand(t)
	escalated := jsontext.Value(`{"command":` + strconv.Quote(command) + `,"cwd":` + strconv.Quote(outside) + `,"escalate":true,"justification":"retry"}`)
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "run-cwd-out-esc", Args: escalated}, env)
	if err != nil {
		t.Fatal(err)
	}
	requireCwdOutput(t, result, outside)
	env.Sandbox.Mode = runner.SandboxOff
	plain := jsontext.Value(`{"command":` + strconv.Quote(command) + `,"cwd":` + strconv.Quote(outside) + `}`)
	result, err = toolExec.Run(t.Context(), tool.Call{ID: "run-cwd-out-off", Args: plain}, env)
	if err != nil {
		t.Fatal(err)
	}
	requireCwdOutput(t, result, outside)
}

type remoteStub struct {
	runner.Runner
	argv []string
}

func (remoteStub) RemoteHost() bool { return true }

func (s *remoteStub) Start(ctx context.Context, spec runner.ProcessSpec) error {
	s.argv = spec.Argv
	return errors.New("remote start stub")
}

func TestExecUsesLinuxShellForRemote(t *testing.T) {
	stub := &remoteStub{}
	env := tool.Env{Runner: stub, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	if _, err := New().Run(t.Context(), tool.Call{ID: "run-remote", Args: jsontext.Value(`{"command":"echo hi"}`)}, env); err == nil {
		t.Fatal("remote stub start accepted")
	}
	want := []string{"sh", "-c", "echo hi"}
	if !slices.Equal(stub.argv, want) {
		t.Fatalf("remote argv = %q, want %q", stub.argv, want)
	}
	if got := shellArgvForHost(stub, "windows", "echo hi"); !slices.Equal(got, want) {
		t.Fatalf("remote shell on windows host = %q, want %q", got, want)
	}
	if got := shellArgvForHost(nil, "windows", "echo hi"); !slices.Equal(got, []string{"cmd", "/C", "echo hi"}) {
		t.Fatalf("local shell on windows host = %q, want cmd", got)
	}
	if got := shellArgvForHost(nil, "linux", "echo hi"); !slices.Equal(got, want) {
		t.Fatalf("local shell on linux host = %q, want %q", got, want)
	}
}

type exitAfterKillRunner struct {
	runner.Runner
	mu     sync.Mutex
	killed bool
	reads  int
	after  int
}

func (f *exitAfterKillRunner) Start(context.Context, runner.ProcessSpec) error { return nil }

func (f *exitAfterKillRunner) Signal(_ context.Context, _ string, signal runner.Signal) error {
	if signal == runner.SignalKill {
		f.mu.Lock()
		f.killed = true
		f.mu.Unlock()
	}
	return nil
}

func (f *exitAfterKillRunner) Read(context.Context, string, int64) (runner.Output, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	out := runner.Output{Path: "runs/exit-after-kill/out"}
	if !f.killed {
		return out, nil
	}
	f.reads++
	if f.after > 0 && f.reads >= f.after {
		out.Exit = &runner.Exit{Code: 137}
	}
	return out, nil
}

func TestExecKilledCallPollsForExitRecord(t *testing.T) {
	fake := &exitAfterKillRunner{after: 3}
	env := tool.Env{Runner: fake, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	toolExec := New()
	toolExec.grace = 300 * time.Millisecond
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "kill-poll", Args: jsontext.Value(`{"command":"true","timeout_ms":100}`)}, env)
	if err == nil || !strings.Contains(err.Error(), "timed out") {
		t.Fatalf("error = %v, want the timeout", err)
	}
	if result.OK {
		t.Fatal("killed call reported success")
	}
	if result.ExitCode == nil || *result.ExitCode != 137 {
		t.Fatalf("killed call settled without the exit record it polled for: exit=%v", result.ExitCode)
	}
}

func TestExecKilledCallWithoutExitReportsError(t *testing.T) {
	fake := &exitAfterKillRunner{}
	env := tool.Env{Runner: fake, Cwd: t.TempDir(), Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}
	toolExec := New()
	toolExec.grace = 300 * time.Millisecond
	toolExec.killWait = 300 * time.Millisecond
	result, err := toolExec.Run(t.Context(), tool.Call{ID: "kill-silent", Args: jsontext.Value(`{"command":"true","timeout_ms":100}`)}, env)
	if err == nil || !strings.Contains(err.Error(), "did not report an exit") {
		t.Fatalf("error = %v, want the missing exit report", err)
	}
	if result.ExitCode != nil {
		t.Fatalf("silent kill reported an exit code: %d", *result.ExitCode)
	}
}
