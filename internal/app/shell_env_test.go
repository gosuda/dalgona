//go:build !windows

package app

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/agent"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/runner"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	os.Exit(m.Run())
}

func loginShellCounter(t *testing.T) string {
	t.Helper()
	counter := filepath.Join(t.TempDir(), "captures")
	shell := filepath.Join(t.TempDir(), "zsh")
	body := "#!/bin/sh\n[ \"$1\" = -lic ] && [ \"$2\" = env ] || exit 3\nprintf x >> '" + counter + "'\nprintf '%s\\n' 'RC_VAR=from_shell' 'MY_API_KEY=private_capture'\n"
	if err := os.WriteFile(shell, []byte(body), 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("SHELL", shell)
	t.Setenv("RC_VAR", "")
	t.Setenv("MY_API_KEY", "")
	return counter
}

func TestHostSharesLoginShellSnapshot(t *testing.T) {
	for _, mode := range []string{"snapshot", "inherit"} {
		t.Run(mode, func(t *testing.T) {
			snapshotCase(t, mode)
		})
	}
}

type snapshotCaseData struct {
	cwd     string
	mode    string
	want    string
	counter string
}

func snapshotCase(t *testing.T, mode string) {
	home, cwd := t.TempDir(), t.TempDir()
	counter := loginShellCounter(t)
	t.Setenv("DALGONA_HOME", home)
	t.Setenv("HOME", t.TempDir())
	if err := os.WriteFile(filepath.Join(home, "config.toml"), []byte("shell_env = \""+mode+"\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	var factory agent.RunnerFactory
	host, closeHost, err := NewHost(t.Context(), Options{Product: config.ProductDalgona, Cwd: cwd, Batteries: func(_ context.Context, base Base) (Batteries, error) {
		factory = base.Local
		return Batteries{}, nil
	}})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = closeHost() })
	want := ""
	if mode == "snapshot" {
		want = "from_shell"
	}
	for _, id := range []string{"first", "second"} {
		thread, startErr := host.Start(t.Context(), agent.ThreadSpec{Cwd: cwd, Model: "openai/test"})
		if startErr != nil {
			t.Fatal(startErr)
		}
		run, runErr := factory(t.Context(), thread.ID(), "local")
		if runErr != nil {
			t.Fatal(runErr)
		}
		assertSnapshotProcess(t, run, id, cwd, want)
	}
	assertSnapshotHostExecs(t, host, snapshotCaseData{cwd: cwd, mode: mode, want: want, counter: counter})
}

func assertSnapshotHostExecs(t *testing.T, host *agent.Host, case_ snapshotCaseData) {
	t.Helper()
	assertSnapshotOverrides(t, host, case_)
	data, err := os.ReadFile(case_.counter)
	if case_.mode == "inherit" && os.IsNotExist(err) {
		return
	}
	if err != nil || string(data) != "x" {
		t.Fatalf("captures = %q, %v", data, err)
	}
	assertSnapshotRefresh(t, host, case_)
}

func assertSnapshotOverrides(t *testing.T, host *agent.Host, case_ snapshotCaseData) {
	t.Helper()
	value := "explicit"
	result, err := host.Exec(t.Context(), "host-exec", agent.ExecSpec{Argv: []string{"/bin/sh", "-c", `printf '%s|%s' "$RC_VAR" "$MY_API_KEY"`}, Dir: case_.cwd, Sandbox: config.SandboxOff, Env: map[string]*string{"RC_VAR": &value, "MY_API_KEY": nil}})
	if err != nil || result.ExitCode != 0 || string(result.Output) != "explicit|" {
		t.Fatalf("host exec = %+v, %v", result, err)
	}
	deleted, err := host.Exec(t.Context(), "host-delete", agent.ExecSpec{Argv: []string{"/bin/sh", "-c", `printf '%s' "${RC_VAR-unset}"`}, Dir: case_.cwd, Sandbox: config.SandboxOff, Env: map[string]*string{"RC_VAR": nil}})
	if err != nil || deleted.ExitCode != 0 || string(deleted.Output) != "unset" {
		t.Fatalf("deleted snapshot variable = %+v, %v", deleted, err)
	}
}

func assertSnapshotRefresh(t *testing.T, host *agent.Host, case_ snapshotCaseData) {
	t.Helper()
	if writeErr := os.WriteFile(filepath.Join(os.Getenv("HOME"), ".zshrc"), []byte("refresh"), 0o600); writeErr != nil {
		t.Fatal(writeErr)
	}
	refreshed, err := host.Exec(t.Context(), "host-refresh", agent.ExecSpec{Argv: []string{"/bin/sh", "-c", `printf '%s' "$RC_VAR"`}, Dir: case_.cwd, Sandbox: config.SandboxOff})
	if err != nil || refreshed.ExitCode != 0 || string(refreshed.Output) != case_.want {
		t.Fatalf("refreshed snapshot = %+v, %v", refreshed, err)
	}
	data, err := os.ReadFile(case_.counter)
	if err != nil || string(data) != "xx" {
		t.Fatalf("captures after rc change = %q, %v", data, err)
	}
}

func assertSnapshotProcess(t *testing.T, run runner.Runner, id, cwd, want string) {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 10*time.Second)
	defer cancel()
	if err := run.Start(ctx, runner.ProcessSpec{ID: id, Argv: []string{"/bin/sh", "-c", `printf '%s|%s' "$RC_VAR" "$MY_API_KEY"`}, Dir: cwd, Sandbox: runner.Sandbox{Mode: runner.SandboxOff}}); err != nil {
		t.Fatal(err)
	}
	var out strings.Builder
	var offset int64
	for {
		page, err := run.Read(ctx, id, offset)
		if err != nil {
			t.Fatal(err)
		}
		out.Write(page.Data)
		offset = page.Next
		if page.Exit != nil {
			if page.Exit.Code != 0 || out.String() != want+"|" {
				t.Fatalf("thread process = %q, exit %+v", out.String(), page.Exit)
			}
			return
		}
		select {
		case <-ctx.Done():
			t.Fatal(ctx.Err())
		case <-time.After(10 * time.Millisecond):
		}
	}
}
