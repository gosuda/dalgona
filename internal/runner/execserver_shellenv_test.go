//go:build !windows

package runner

import (
	"bytes"
	"net"
	"path/filepath"
	"strings"
	"testing"
)

func TestParseExecServerShellEnv(t *testing.T) {
	for _, row := range []parseRow{
		{name: "default", mode: "snapshot"},
		{name: "inherit", args: []string{"--shell-env", "inherit"}, mode: "inherit", set: true},
		{name: "snapshot equals form", args: []string{"--shell-env=snapshot"}, mode: "snapshot", set: true},
		{name: "invalid", args: []string{"--shell-env", "bogus"}, err: "exec-server: shell-env must be one of snapshot, inherit"},
	} {
		t.Run(row.name, func(t *testing.T) {
			checkParseRow(t, row)
		})
	}
}

type parseRow struct {
	name string
	args []string
	mode string
	set  bool
	err  string
}

func checkParseRow(t *testing.T, row parseRow) {
	t.Helper()
	cfg, err := parseExecServerConfig(row.args)
	if row.err != "" {
		if err == nil || err.Error() != row.err {
			t.Fatalf("error = %v, want %q", err, row.err)
		}
		return
	}
	if err != nil {
		t.Fatal(err)
	}
	if cfg.shellEnv != row.mode || cfg.shellEnvSet != row.set {
		t.Fatalf("shell-env = %q set %v, want %q set %v", cfg.shellEnv, cfg.shellEnvSet, row.mode, row.set)
	}
}

func TestExecServerSnapshotSelection(t *testing.T) {
	t.Setenv("SHELL", "/opt/test-shell")
	const missing = "exec-server: snapshot requires a secret-name predicate"
	for _, row := range []snapshotRow{
		{name: "default without predicate inherits", args: nil, secret: nil},
		{name: "default with predicate snapshots", args: nil, secret: fakeSecretName, snapshot: true},
		{name: "explicit snapshot with predicate", args: []string{"--shell-env", "snapshot"}, secret: fakeSecretName, snapshot: true},
		{name: "explicit snapshot without predicate fails", args: []string{"--shell-env", "snapshot"}, secret: nil, err: missing},
		{name: "explicit inherit with predicate", args: []string{"--shell-env", "inherit"}, secret: fakeSecretName},
		{name: "explicit inherit without predicate", args: []string{"--shell-env", "inherit"}, secret: nil},
	} {
		t.Run(row.name, func(t *testing.T) {
			checkSnapshotRow(t, row)
		})
	}
}

type snapshotRow struct {
	name     string
	args     []string
	secret   func(string) bool
	snapshot bool
	err      string
}

func checkSnapshotRow(t *testing.T, row snapshotRow) {
	t.Helper()
	cfg, err := parseExecServerConfig(row.args)
	if err != nil {
		t.Fatal(err)
	}
	snapshot, err := execServerSnapshot(cfg, row.secret)
	if row.err != "" {
		if err == nil || err.Error() != row.err {
			t.Fatalf("error = %v, want %q", err, row.err)
		}
		return
	}
	if err != nil {
		t.Fatal(err)
	}
	if (snapshot != nil) != row.snapshot {
		t.Fatalf("snapshot = %v, want present %v", snapshot, row.snapshot)
	}
	if snapshot != nil && snapshot.shell != "/opt/test-shell" {
		t.Fatalf("snapshot shell = %q, want the SHELL value", snapshot.shell)
	}
}

func TestExecServerMainShellEnvDiagnostics(t *testing.T) {
	for _, row := range []struct {
		name string
		arg  string
		want string
	}{
		{name: "snapshot without predicate", arg: "snapshot", want: "exec-server: snapshot requires a secret-name predicate\n"},
		{name: "invalid value", arg: "bogus", want: "exec-server: shell-env must be one of snapshot, inherit\n"},
	} {
		t.Run(row.name, func(t *testing.T) {
			var stdout bytes.Buffer
			code := ExecServerMain([]string{"--shell-env", row.arg, "--root", t.TempDir()}, strings.NewReader(""), &stdout)
			if code != 2 || stdout.String() != row.want {
				t.Fatalf("exit %d with %q, want exit 2 with %q", code, stdout.String(), row.want)
			}
		})
	}
}

func startExecServerMain(t *testing.T, args []string, isSecret func(string) bool) Runner {
	t.Helper()
	server, client := net.Pipe()
	exit := make(chan int, 1)
	go func() { exit <- ExecServerMainWithBridge(args, server, server, nil, isSecret) }()
	t.Cleanup(func() {
		_ = client.Close()
		if code := <-exit; code != 0 {
			t.Errorf("exec server exited with %d", code)
		}
	})
	return newRemoteContract(t, client)
}

func TestExecServerMainCapturesLoginShellOnce(t *testing.T) {
	for _, row := range []struct {
		name string
		flag []string
		want string
		runs int
	}{
		{name: "snapshot", want: "loaded_from_rc", runs: 1},
		{name: "inherit", flag: []string{"--shell-env", "inherit"}, want: "", runs: 0},
	} {
		t.Run(row.name, func(t *testing.T) {
			counter := filepath.Join(t.TempDir(), "count")
			t.Setenv("SHELL", writeLoginShell(t, filepath.Join(t.TempDir(), "zsh"), counter, loginShellEnvironment))
			t.Setenv("HOME", t.TempDir())
			t.Chdir(t.TempDir())
			remote := startExecServerMain(t, append([]string{"--root", t.TempDir()}, row.flag...), fakeSecretName)
			for _, id := range []string{"first", "second"} {
				spec := ProcessSpec{ID: id, Argv: []string{"sh", "-c", `printf '%s' "$RC_VAR"`}, Dir: t.TempDir()}
				if err := remote.Start(t.Context(), spec); err != nil {
					t.Fatal(err)
				}
				if got := string(readContractOutput(t, remote, id).Data); got != row.want {
					t.Fatalf("%s saw RC_VAR=%q, want %q", id, got, row.want)
				}
			}
			requireLoginShellRuns(t, counter, row.runs, "two remote starts")
		})
	}
}
