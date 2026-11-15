//go:build !windows

package runner

import (
	"bytes"
	"context"
	"errors"
	"io/fs"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

const loginShellEnvironment = `printf '%s\n' 'Last login: today' 'PATH=/bin' 'SAME=1' 'CHANGED=new' 'RC_VAR=loaded_from_rc' 'MY_API_KEY=supersecret' 'GITHUB_TOKEN=x'
`

func fakeSecretName(name string) bool {
	return strings.HasSuffix(name, "_API_KEY") || strings.HasSuffix(name, "_TOKEN")
}

func writeLoginShell(t *testing.T, path, counter, body string) string {
	t.Helper()
	script := "#!/bin/sh\n" +
		`[ "$#" = 2 ] && [ "$1" = -lic ] && [ "$2" = env ] || exit 3` + "\n" +
		"echo x >> '" + counter + "'\n" + body
	if err := os.WriteFile(path, []byte(script), 0o700); err != nil {
		t.Fatal(err)
	}
	return path
}

func loginShellRuns(t *testing.T, counter string) int {
	t.Helper()
	data, err := os.ReadFile(counter)
	if errors.Is(err, fs.ErrNotExist) {
		return 0
	}
	if err != nil {
		t.Fatal(err)
	}
	return strings.Count(string(data), "\n")
}

func requireLoginShellRuns(t *testing.T, counter string, want int, step string) {
	t.Helper()
	if got := loginShellRuns(t, counter); got != want {
		t.Fatalf("%s: login shell ran %d times, want %d", step, got, want)
	}
}

func newTestSnapshot(t *testing.T, shell string) (*ShellEnvSnapshot, *bytes.Buffer) {
	t.Helper()
	logs := &bytes.Buffer{}
	snapshot := NewShellEnvSnapshot(shell, fakeSecretName)
	snapshot.log = slog.New(slog.NewTextHandler(logs, nil))
	return snapshot, logs
}

func sortedCopy(values []string) []string {
	return slices.Sorted(slices.Values(values))
}

func TestShellEnvSnapshot(t *testing.T) {
	t.Run("keeps only changed variables", testShellEnvKeepsChanged)
	t.Run("startup file changes refresh the snapshot", testShellEnvFingerprint)
	t.Run("failure is cached until a startup file changes", testShellEnvFailureCached)
	t.Run("timeout returns the daemon environment", testShellEnvTimeout)
	t.Run("overflow and malformed output are refused", testShellEnvRejectsOutput)
	t.Run("caller cancellation kills the capture group and is not cached", testShellEnvCancel)
	t.Run("concurrent callers capture once", testShellEnvConcurrent)
	t.Run("a snapshot without a predicate captures nothing", testShellEnvNoPredicate)
	t.Run("an empty shell name selects the system shell", func(t *testing.T) {
		if got := NewShellEnvSnapshot("", fakeSecretName).shellPath(); got != "/bin/sh" {
			t.Fatalf("shell path = %q, want /bin/sh", got)
		}
	})
}

func testShellEnvKeepsChanged(t *testing.T) {
	home, counter := t.TempDir(), filepath.Join(t.TempDir(), "runs")
	shell := writeLoginShell(t, filepath.Join(t.TempDir(), "zsh"), counter, loginShellEnvironment)
	snapshot, logs := newTestSnapshot(t, shell)
	daemon := []string{"PATH=/bin", "SAME=1", "CHANGED=old", "HOME=" + home}
	got := snapshot.Resolve(t.Context(), daemon)
	want := []string{"CHANGED=new", "HOME=" + home, "PATH=/bin", "RC_VAR=loaded_from_rc", "SAME=1"}
	if !slices.Equal(sortedCopy(got), want) {
		t.Fatalf("resolved environment = %q, want %q", sortedCopy(got), want)
	}
	if delta := []string{"CHANGED=new", "RC_VAR=loaded_from_rc"}; !slices.Equal(snapshot.env, delta) {
		t.Fatalf("cached delta = %q, want %q", snapshot.env, delta)
	}
	if !slices.Contains(daemon, "CHANGED=old") {
		t.Fatalf("daemon environment changed: %q", daemon)
	}
	got[0] = "PATH=mutated"
	if again := snapshot.Resolve(t.Context(), daemon); !slices.Contains(again, "PATH=/bin") || slices.Contains(again, "PATH=mutated") {
		t.Fatalf("a caller edit leaked into the next result: %q", again)
	}
	if logs.Len() != 0 {
		t.Fatalf("a successful capture logged: %s", logs)
	}
	requireLoginShellRuns(t, counter, 1, "two resolves")
}

func testShellEnvFingerprint(t *testing.T) {
	for _, row := range []struct {
		shell   string
		file    string
		refresh bool
	}{
		{"zsh", ".zshenv", true},
		{"zsh", ".zprofile", true},
		{"zsh", ".zshrc", true},
		{"zsh", ".zlogin", true},
		{"bash", ".bash_profile", true},
		{"bash", ".bash_login", true},
		{"bash", ".profile", true},
		{"bash", ".bashrc", true},
		{"sh", ".profile", true},
		{"sh", ".zshrc", false},
		{"sh", ".bashrc", false},
		{"zsh", ".bashrc", false},
	} {
		t.Run(row.shell+row.file, func(t *testing.T) {
			home, counter := t.TempDir(), filepath.Join(t.TempDir(), "runs")
			shell := writeLoginShell(t, filepath.Join(t.TempDir(), row.shell), counter, loginShellEnvironment)
			snapshot, _ := newTestSnapshot(t, shell)
			daemon := []string{"PATH=/bin", "HOME=" + home}
			snapshot.Resolve(t.Context(), daemon)
			snapshot.Resolve(t.Context(), daemon)
			want := 1
			requireLoginShellRuns(t, counter, want, "unchanged")
			rc := filepath.Join(home, row.file)
			for step, edit := range []func() error{
				func() error { return os.WriteFile(rc, []byte("a"), 0o600) },
				func() error { return os.WriteFile(rc, []byte("ab"), 0o600) },
				func() error { return os.Remove(rc) },
			} {
				if err := edit(); err != nil {
					t.Fatal(err)
				}
				snapshot.Resolve(t.Context(), daemon)
				if row.refresh {
					want++
				}
				requireLoginShellRuns(t, counter, want, "edit "+strconv.Itoa(step))
			}
		})
	}
}

func testShellEnvFailureCached(t *testing.T) {
	home, counter := t.TempDir(), filepath.Join(t.TempDir(), "runs")
	shell := writeLoginShell(t, filepath.Join(t.TempDir(), "zsh"), counter, "printf '%s\\n' 'MY_API_KEY=supersecret'\nexit 1\n")
	snapshot, logs := newTestSnapshot(t, shell)
	daemon := []string{"PATH=/bin", "HOME=" + home}
	for range 2 {
		if got := snapshot.Resolve(t.Context(), daemon); !slices.Equal(got, daemon) {
			t.Fatalf("failed capture returned %q, want the daemon environment %q", got, daemon)
		}
	}
	requireLoginShellRuns(t, counter, 1, "cached failure")
	text := logs.String()
	for _, want := range []string{"level=WARN", "class=exit", "status=1"} {
		if !strings.Contains(text, want) {
			t.Fatalf("warning %q lacks %q", text, want)
		}
	}
	if strings.Contains(text, "supersecret") || strings.Count(text, "level=WARN") != 1 {
		t.Fatalf("warning must appear once and hold no capture bytes: %q", text)
	}
	if err := os.WriteFile(filepath.Join(home, ".zshrc"), []byte("changed"), 0o600); err != nil {
		t.Fatal(err)
	}
	snapshot.Resolve(t.Context(), daemon)
	requireLoginShellRuns(t, counter, 2, "after a startup file change")
	if got := strings.Count(logs.String(), "level=WARN"); got != 2 {
		t.Fatalf("warnings after the refresh = %d, want 2", got)
	}
}

func testShellEnvTimeout(t *testing.T) {
	home, counter := t.TempDir(), filepath.Join(t.TempDir(), "runs")
	shell := writeLoginShell(t, filepath.Join(t.TempDir(), "zsh"), counter, "sleep 30\n")
	snapshot, logs := newTestSnapshot(t, shell)
	snapshot.timeout = 50 * time.Millisecond
	daemon := []string{"PATH=" + os.Getenv("PATH"), "HOME=" + home}
	if got := snapshot.Resolve(t.Context(), daemon); !slices.Equal(got, daemon) {
		t.Fatalf("timed out capture returned %q, want the daemon environment %q", got, daemon)
	}
	text := logs.String()
	if !strings.Contains(text, "level=WARN") || !strings.Contains(text, "class=timeout") || strings.Contains(text, "status=") {
		t.Fatalf("timeout warning = %q", text)
	}
}

func testShellEnvRejectsOutput(t *testing.T) {
	for _, row := range []struct {
		name  string
		body  string
		limit int
		class string
	}{
		{"overflow", loginShellEnvironment, 16, "class=overflow"},
		{"nul byte", "printf 'A=1\\0B=2\\n'\n", 0, "class=malformed"},
		{"invalid name", "printf 'GOOD=1\\n9INVALID=secret\\n'\n", 0, "class=malformed"},
		{"no record", "printf 'Last login: today\\nnot an environment line\\n'\n", 0, "class=malformed"},
	} {
		t.Run(row.name, func(t *testing.T) {
			home, counter := t.TempDir(), filepath.Join(t.TempDir(), "runs")
			shell := writeLoginShell(t, filepath.Join(t.TempDir(), "zsh"), counter, row.body)
			snapshot, logs := newTestSnapshot(t, shell)
			if row.limit > 0 {
				snapshot.limit = row.limit
			}
			daemon := []string{"PATH=/bin", "HOME=" + home}
			if got := snapshot.Resolve(t.Context(), daemon); !slices.Equal(got, daemon) {
				t.Fatalf("refused capture returned %q, want the daemon environment %q", got, daemon)
			}
			if text := logs.String(); !strings.Contains(text, row.class) || strings.Contains(text, "supersecret") {
				t.Fatalf("warning = %q, want %s and no capture bytes", text, row.class)
			}
		})
	}
}

func waitForShellPID(t *testing.T, path string) int {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for {
		data, err := os.ReadFile(path)
		if err == nil && bytes.HasSuffix(data, []byte("\n")) {
			pid, parseErr := strconv.Atoi(strings.TrimSpace(string(data)))
			if parseErr != nil {
				t.Fatalf("pid file %q: %v", data, parseErr)
			}
			return pid
		}
		if time.Now().After(deadline) {
			t.Fatalf("login shell wrote no pid to %s", path)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func testShellEnvCancel(t *testing.T) {
	home, dir := t.TempDir(), t.TempDir()
	counter, pidFile, shell := filepath.Join(dir, "runs"), filepath.Join(dir, "pid"), filepath.Join(dir, "zsh")
	writeLoginShell(t, shell, counter, "sleep 300 &\necho $! > '"+pidFile+"'\nwait\n")
	snapshot, logs := newTestSnapshot(t, shell)
	daemon := []string{"PATH=" + os.Getenv("PATH"), "HOME=" + home}
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	result := make(chan []string, 1)
	go func() { result <- snapshot.Resolve(ctx, daemon) }()
	pid := waitForShellPID(t, pidFile)
	cancel()
	if got := <-result; !slices.Equal(got, daemon) {
		t.Fatalf("canceled capture returned %q, want the daemon environment %q", got, daemon)
	}
	if !descendantStopped(pid) {
		t.Fatalf("descendant %d of the login shell survived the cancel", pid)
	}
	if logs.Len() != 0 {
		t.Fatalf("a caller cancel logged a warning: %s", logs)
	}
	writeLoginShell(t, shell, counter, loginShellEnvironment)
	if got := snapshot.Resolve(t.Context(), daemon); !slices.Contains(got, "RC_VAR=loaded_from_rc") {
		t.Fatalf("a canceled capture was cached: %q", got)
	}
}

func testShellEnvConcurrent(t *testing.T) {
	home, counter := t.TempDir(), filepath.Join(t.TempDir(), "runs")
	shell := writeLoginShell(t, filepath.Join(t.TempDir(), "zsh"), counter, loginShellEnvironment)
	snapshot, _ := newTestSnapshot(t, shell)
	daemon := []string{"PATH=/bin", "HOME=" + home}
	results := make([][]string, 8)
	var wg sync.WaitGroup
	for i := range results {
		wg.Go(func() { results[i] = snapshot.Resolve(t.Context(), daemon) })
	}
	wg.Wait()
	requireLoginShellRuns(t, counter, 1, "eight concurrent resolves")
	for i, got := range results {
		if !slices.Contains(got, "RC_VAR=loaded_from_rc") {
			t.Fatalf("caller %d lacks the snapshot: %q", i, got)
		}
	}
}

func testShellEnvNoPredicate(t *testing.T) {
	home, counter := t.TempDir(), filepath.Join(t.TempDir(), "runs")
	shell := writeLoginShell(t, filepath.Join(t.TempDir(), "zsh"), counter, loginShellEnvironment)
	daemon := []string{"PATH=/bin", "HOME=" + home}
	for name, snapshot := range map[string]*ShellEnvSnapshot{
		"nil predicate": NewShellEnvSnapshot(shell, nil),
		"nil snapshot":  nil,
	} {
		got := snapshot.Resolve(t.Context(), daemon)
		if !slices.Equal(got, daemon) {
			t.Fatalf("%s returned %q, want the daemon environment %q", name, got, daemon)
		}
		got[0] = "PATH=mutated"
		if daemon[0] != "PATH=/bin" {
			t.Fatalf("%s returned the daemon slice itself", name)
		}
	}
	requireLoginShellRuns(t, counter, 0, "no predicate")
}

func TestLocalStartAppliesShellEnvironment(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("CHANGED", "old")
	pathEnv := func(extra ...string) []string {
		return append([]string{"PATH=" + os.Getenv("PATH")}, extra...)
	}
	for _, row := range []struct {
		name     string
		spec     ProcessSpec
		snapshot bool
		want     string
		runs     int
	}{
		{"snapshot overlays the daemon environment", ProcessSpec{}, true, "loaded_from_rc|new|", 1},
		{"explicit environment wins", ProcessSpec{Env: pathEnv("CHANGED=explicit", "KEEP=yes")}, true, "loaded_from_rc|explicit|yes", 1},
		{"resolved environment is used unchanged", ProcessSpec{Env: pathEnv("KEEP=resolved"), EnvResolved: true}, true, "||resolved", 0},
		{"inherit leaves the daemon environment", ProcessSpec{}, false, "|old|", 0},
	} {
		t.Run(row.name, func(t *testing.T) {
			dir := t.TempDir()
			counter := filepath.Join(dir, "count")
			shell := writeLoginShell(t, filepath.Join(dir, "zsh"), counter, loginShellEnvironment)
			local, err := NewLocal(filepath.Join(dir, "runs"))
			if row.snapshot {
				snapshot, _ := newTestSnapshot(t, shell)
				local, err = NewLocalWithShellEnv(filepath.Join(dir, "runs"), snapshot)
			}
			if err != nil {
				t.Fatal(err)
			}
			spec := row.spec
			spec.ID, spec.Dir = "env", dir
			spec.Argv = []string{"sh", "-c", `printf '%s|%s|%s' "$RC_VAR" "$CHANGED" "$KEEP"`}
			spec.Sandbox = Sandbox{Mode: SandboxOff}
			startTestProcess(t, local, spec)
			if got := string(waitForExit(t, local, "env").Data); got != row.want {
				t.Fatalf("child saw %q, want %q", got, row.want)
			}
			requireLoginShellRuns(t, counter, row.runs, "start")
		})
	}
}

func TestRemoteStartRefusesResolvedEnvironment(t *testing.T) {
	server, client := net.Pipe()
	local, err := NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() { done <- Serve(t.Context(), server, local) }()
	t.Cleanup(func() { stopContractServer(t, server, done) })
	remote := newRemoteContract(t, client)
	spec := ProcessSpec{ID: "resolved", Argv: []string{"sh", "-c", "true"}, Dir: t.TempDir(), EnvResolved: true}
	if err := remote.Start(t.Context(), spec); !errors.Is(err, ErrResolvedEnvironment) {
		t.Fatalf("Start with a resolved environment = %v, want %v", err, ErrResolvedEnvironment)
	}
	if _, err := local.Read(t.Context(), spec.ID, 0); !errors.Is(err, ErrProcessNotFound) {
		t.Fatalf("the server saw the refused process: %v", err)
	}
	spec.EnvResolved = false
	if err := remote.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	if state := readContractOutput(t, remote, spec.ID); state.Exit == nil || state.Exit.Code != 0 {
		t.Fatalf("unresolved start exit = %+v", state.Exit)
	}
}
