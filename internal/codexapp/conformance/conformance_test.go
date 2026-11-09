package conformance

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

const (
	externalTimeout = 5 * time.Minute
	cargoTimeout    = 30 * time.Minute
	flowReserve     = 2 * time.Minute
	stopGrace       = 5 * time.Second
	lockWait        = 2 * time.Minute
)

var (
	errNoDaemon = errors.New("no daemon process")
	errLockBusy = errors.New("lock held by another process")
)

type fixture struct {
	t         *testing.T
	root      string
	home      string
	workspace string
	binary    string
	model     *fakeModel
}

func requirePrerequisites(t *testing.T, source string, tools ...string) {
	t.Helper()
	var missing []string
	if !processTableAvailable {
		missing = append(missing, "/proc process table for daemon cleanup")
	}
	if _, err := os.Stat(filepath.Join(repositoryRoot(t), source)); err != nil {
		missing = append(missing, "reference source "+source+" (.references is not checked out)")
	}
	for _, name := range tools {
		if _, err := exec.LookPath(name); err != nil {
			missing = append(missing, name+" on PATH")
		}
	}
	if len(missing) > 0 {
		t.Skip("missing prerequisite: " + strings.Join(missing, ", "))
	}
}

func lockCache(t *testing.T, dir string) {
	t.Helper()
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatalf("mkdir %s: %v", dir, err)
	}
	release, err := lockFile(dir+".lock", lockWait)
	if errors.Is(err, errLockBusy) {
		t.Skipf("missing prerequisite: exclusive use of %s; another conformance run held it for %v", dir, lockWait)
	}
	if err != nil {
		t.Fatalf("lock %s: %v", dir, err)
	}
	t.Cleanup(func() {
		if err := release(); err != nil {
			t.Errorf("unlock %s: %v", dir, err)
		}
	})
}

func newFixture(t *testing.T, approval string) *fixture {
	t.Helper()
	dir := t.TempDir()
	f := &fixture{
		t: t, root: repositoryRoot(t), home: filepath.Join(dir, "home"), workspace: filepath.Join(dir, "workspace"),
		binary: filepath.Join(dir, "dalgona"), model: newFakeModel(t),
	}
	writeFile(t, filepath.Join(f.home, "config.toml"), "model = \"openai/gpt-test\"\napproval = \""+approval+"\"\nsandbox = \"workspace-write\"\n")
	for _, dir := range []string{f.workspace, filepath.Join(f.home, "codex")} {
		if err := os.MkdirAll(dir, 0o700); err != nil {
			t.Fatalf("mkdir %s: %v", dir, err)
		}
	}
	f.run(t, externalTimeout, f.root, "go", "build", "-o", f.binary, "./cmd/dalgona")
	t.Cleanup(f.stopDaemon)
	return f
}

func repositoryRoot(t *testing.T) string {
	t.Helper()
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("locate conformance test source")
	}
	return filepath.Clean(filepath.Join(filepath.Dir(file), "..", "..", ".."))
}

func writeFile(t *testing.T, path, body string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatalf("mkdir %s: %v", filepath.Dir(path), err)
	}
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func cacheDir(name string) string {
	base, err := os.UserCacheDir()
	if err != nil {
		base = os.TempDir()
	}
	return filepath.Join(base, "dalgona-conformance", name)
}

func (f *fixture) env() []string {
	overrides := map[string]string{
		"DALGONA_HOME":           f.home,
		"OPENAI_BASE_URL":        f.model.URL(),
		"OPENAI_API_KEY":         "conformance-test-key",
		"CODEX_HOME":             filepath.Join(f.home, "codex"),
		"CONFORMANCE_CODEX_BIN":  f.binary,
		"CONFORMANCE_SDK_DIR":    filepath.Join(f.root, ".references", "codex", "sdk", "python"),
		"CONFORMANCE_WORKSPACE":  f.workspace,
		"UV_PROJECT_ENVIRONMENT": cacheDir("uv-env"),
	}
	env := make([]string, 0, len(os.Environ())+len(overrides))
	for _, pair := range os.Environ() {
		key, _, _ := strings.Cut(pair, "=")
		if _, replaced := overrides[key]; !replaced {
			env = append(env, pair)
		}
	}
	for key, value := range overrides {
		env = append(env, key+"="+value)
	}
	return env
}

func (f *fixture) run(t *testing.T, timeout time.Duration, dir, name string, args ...string) string {
	t.Helper()
	ctx, cancel := context.WithTimeoutCause(context.Background(), timeout, errors.New(name+" exceeded "+timeout.String()))
	defer cancel()
	cmd := exec.CommandContext(ctx, name, args...)
	cmd.Dir, cmd.Env, cmd.WaitDelay = dir, f.env(), 10*time.Second
	out, err := cmd.CombinedOutput()
	if cause := context.Cause(ctx); cause != nil {
		t.Fatalf("%s %s: %v\n%s\n%s", name, strings.Join(args, " "), cause, out, f.daemonLog())
	}
	if err != nil {
		t.Fatalf("%s %s: %v\n%s\n%s", name, strings.Join(args, " "), err, out, f.daemonLog())
	}
	return string(out)
}

func (f *fixture) daemonLog() string {
	data, err := os.ReadFile(filepath.Join(f.home, "daemon.log"))
	if err != nil {
		return "daemon.log: " + err.Error()
	}
	return "daemon.log:\n" + string(data)
}

func (f *fixture) stopDaemon() {
	pids, err := daemonPIDs(f.binary, f.home)
	if err != nil {
		if !errors.Is(err, errNoDaemon) {
			f.t.Errorf("find daemon processes: %v", err)
		}
		return
	}
	for _, pid := range pids {
		if err := terminate(pid); err != nil {
			f.t.Errorf("stop daemon: %v", err)
		}
	}
	deadline := time.Now().Add(stopGrace)
	for _, pid := range pids {
		f.reap(pid, deadline)
	}
}

func (f *fixture) reap(pid int, deadline time.Time) {
	if waitPID(pid, deadline) {
		return
	}
	f.t.Errorf("daemon %d ignored SIGTERM for %v", pid, stopGrace)
	if err := kill(pid); err != nil {
		f.t.Errorf("kill daemon: %v", err)
	}
	if !waitPID(pid, time.Now().Add(stopGrace)) {
		f.t.Errorf("daemon %d survived SIGKILL", pid)
	}
}

func (f *fixture) python(t *testing.T, args ...string) string {
	t.Helper()
	driver := filepath.Join(f.root, "internal", "codexapp", "conformance", "testdata", args[0])
	argv := append([]string{"run", "--project", filepath.Join(f.root, ".references", "codex", "sdk", "python"), "python", driver}, args[1:]...)
	return f.run(t, externalTimeout, f.workspace, "uv", argv...)
}

func expectOutput(t *testing.T, out string, want ...string) {
	t.Helper()
	for _, text := range want {
		if !strings.Contains(out, text) {
			t.Fatalf("output lacks %q:\n%s", text, out)
		}
	}
}

func TestCodexPythonSDK(t *testing.T) {
	requirePrerequisites(t, filepath.Join(".references", "codex", "sdk", "python", "examples"), "uv")
	lockCache(t, cacheDir("uv-env"))
	f := newFixture(t, "auto")
	examples := []struct {
		name string
		want []string
	}{
		{"01_quickstart_constructor", []string{"Server: dalgona", "Text: " + replyText}},
		{"02_turn_run", []string{"status: TurnStatus.completed", "text: " + replyText}},
		{"03_turn_stream_events", []string{"stream.started.seen: True", "stream.completed: completed", replyText}},
		{"05_existing_thread", []string{"Created thread:", replyText}},
	}
	for _, example := range examples {
		t.Run(example.name, func(t *testing.T) {
			expectOutput(t, f.python(t, "example.py", example.name), example.want...)
		})
	}
	t.Run("command_approval", func(t *testing.T) {
		out := f.python(t, "approval.py", approvalMarker+" run the approval command")
		expectOutput(t, out, "approval requests: 1", "command output: "+approvalOutput, "final: "+afterApprovalTxt)
	})
}

func (f *fixture) rustClient(t *testing.T) string {
	t.Helper()
	codex := filepath.Join(f.root, ".references", "codex")
	commit := strings.TrimSpace(f.run(t, externalTimeout, codex, "git", "rev-parse", "HEAD"))
	client := cacheDir("codex-app-server-test-client-" + commit)
	if _, err := os.Stat(client); err == nil {
		return client
	}
	target := cacheDir("cargo-target")
	lockCache(t, target)
	if _, err := os.Stat(client); err == nil {
		return client
	}
	budget := cargoTimeout
	if deadline, ok := t.Deadline(); ok {
		budget = min(budget, time.Until(deadline)-flowReserve)
	}
	if budget < cargoTimeout {
		t.Skipf("missing prerequisite: %s built for codex %s; building it needs up to %v and the test deadline leaves %v, so run once with -timeout 60m", client, commit, cargoTimeout, budget)
	}
	f.run(t, budget, filepath.Join(codex, "codex-rs"), "cargo", "build", "--locked", "-p", "codex-app-server-test-client", "--target-dir", target)
	installFile(t, filepath.Join(target, "debug", "codex-app-server-test-client"), client)
	return client
}

func installFile(t *testing.T, from, to string) {
	t.Helper()
	data, err := os.ReadFile(from)
	if err != nil {
		t.Fatalf("read built client: %v", err)
	}
	partial := to + ".partial"
	if err := os.WriteFile(partial, data, 0o700); err != nil {
		t.Fatalf("write cached client: %v", err)
	}
	if err := os.Rename(partial, to); err != nil {
		t.Fatalf("install cached client: %v", err)
	}
}

func TestCodexAppServerTestClient(t *testing.T) {
	requirePrerequisites(t, filepath.Join(".references", "codex", "codex-rs", "app-server-test-client"), "git", "cargo", "rustc")
	f := newFixture(t, "ask")
	client := f.rustClient(t)
	t.Run("send-message-v2", func(t *testing.T) {
		out := f.run(t, externalTimeout, f.workspace, client, "--codex-bin", f.binary, "send-message-v2", "Say hello.")
		expectOutput(t, out, replyText, "turn/completed notification: Completed")
	})
	t.Run("trigger-cmd-approval", func(t *testing.T) {
		out := f.run(t, externalTimeout, f.workspace, client, "--codex-bin", f.binary, "trigger-cmd-approval", approvalMarker+" run the approval command")
		expectOutput(t, out, "commandExecution approval requested", afterApprovalTxt, "turn/completed notification: Completed")
	})
}
