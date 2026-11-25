package main

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func cloudHome(t *testing.T, configBody string) {
	t.Helper()
	home := t.TempDir()
	for name, body := range map[string]string{"config.toml": configBody, "work.token": "relay-token\n"} {
		if err := os.WriteFile(filepath.Join(home, name), []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	t.Setenv("DALGONA_HOME", home)
}

func runCLI(t *testing.T, args ...string) (int, string, string) {
	t.Helper()
	var stdout, stderr bytes.Buffer
	code := run(t.Context(), args, stdio{in: strings.NewReader(""), out: &stdout, errOut: &stderr})
	return code, stdout.String(), stderr.String()
}

func TestBoxCommandsRefuseADaemonRemote(t *testing.T) {
	cloudHome(t, "[remotes.work]\nkind = \"daemon\"\nurl = \"https://work.example\"\ntoken_file = \"work.token\"\n")
	refusal := "remote: work is a daemon remote and has no box; connect to it from the desktop client\n"
	for _, args := range [][]string{{"cloud", "up", "work", "--copy-auth"}, {"cloud", "down", "work", "--yes"}, {"cloud", "pull", "work"}} {
		if code, _, stderr := runCLI(t, args...); code != 1 || stderr != refusal {
			t.Fatalf("dalgona %s: exit %d, stderr %q; want 1 and %q", strings.Join(args, " "), code, stderr, refusal)
		}
	}
	if code, _, stderr := runCLI(t, "--remote", "work", "pull", "thread-1"); code != 1 || stderr != "dalgona: "+refusal {
		t.Fatalf("dalgona --remote work pull: exit %d, stderr %q", code, stderr)
	}
}

func TestCloudUpRefusesCloudflareBeforeAnyServiceCall(t *testing.T) {
	cloudHome(t, "[runners.edge]\nservice = \"cloudflare\"\nurl = \"https://worker.example\"\n")
	code, stdout, stderr := runCLI(t, "cloud", "up", "edge")
	wantErr := "cloudflare boxes lose their files when the container stops; use sandbox0, modal, sprites, or docker for fully cloud mode\n"
	if code != 1 || stderr != wantErr {
		t.Fatalf("exit %d, stderr %q; want 1 and %q", code, stderr, wantErr)
	}
	if want := "sign in inside the box with `dalgona --remote edge login`\n"; stdout != want {
		t.Fatalf("stdout %q, want %q", stdout, want)
	}
}

func TestCloudRejectsMalformedArguments(t *testing.T) {
	cloudHome(t, "")
	for _, args := range [][]string{{"cloud", "up"}, {"cloud", "down", "work", "--copy-auth"}, {"cloud", "sync", "work"}} {
		code, _, stderr := runCLI(t, args...)
		if want := "usage: dalgona cloud up <name> [--copy-auth] | down <name> [--yes] | pull <name>\n"; code != 2 || stderr != want {
			t.Fatalf("dalgona %s: exit %d, stderr %q", strings.Join(args, " "), code, stderr)
		}
	}
}
