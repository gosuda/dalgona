package main

import (
	"bytes"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func doctorHome(t *testing.T) string {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("the home mode check runs on unix only")
	}
	home := filepath.Join(shortDaemonHome(t), "home")
	if err := os.Mkdir(home, 0o700); err != nil {
		t.Fatal(err)
	}
	t.Setenv("DALGONA_HOME", home)
	for _, name := range []string{
		"OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY", "OPENROUTER_API_KEY", "DEVIN_API_KEY",
		"CHATGPT_API_KEY", "ANTIGRAVITY_API_KEY", "CURSOR_API_KEY",
	} {
		t.Setenv(name, "")
	}
	return home
}

func TestDoctorRejectsOtherArguments(t *testing.T) {
	for _, args := range [][]string{{"doctor", "--bogus"}, {"doctor", "x"}, {"doctor", "--online", "--online"}, {"doctor", "--online", "x"}} {
		var stdout, stderr bytes.Buffer
		code := run(t.Context(), args, stdio{in: strings.NewReader(""), out: &stdout, errOut: &stderr})
		if code != 2 || stderr.String() != "usage: dalgona doctor [--online]\n" || stdout.Len() != 0 {
			t.Fatalf("%q: exit %d, stdout %q, stderr %q; want exit 2 and the usage line only", args, code, stdout.String(), stderr.String())
		}
	}
}

func TestDoctorPrintsOneRowPerCheck(t *testing.T) {
	home := doctorHome(t)
	for _, args := range [][]string{{"doctor"}, {"doctor", "--online"}} {
		var stdout, stderr bytes.Buffer
		code := run(t.Context(), args, stdio{in: strings.NewReader(""), out: &stdout, errOut: &stderr})
		lines := strings.Split(strings.TrimRight(stdout.String(), "\n"), "\n")
		if code != 0 || stderr.Len() != 0 || lines[0] != "ok home: "+home {
			t.Fatalf("%q: exit %d, stderr %q, first row %q", args, code, stderr.String(), lines[0])
		}
		for _, line := range lines {
			checkDoctorRow(t, args, line)
		}
	}
}

func checkDoctorRow(t *testing.T, args []string, line string) {
	t.Helper()
	status, rest, _ := strings.Cut(line, " ")
	check, _, found := strings.Cut(rest, ": ")
	if (status != "ok" && status != "warn" && status != "fail") || !found || check == "" {
		t.Fatalf("%q: row %q is not <status> <check>: <detail>", args, line)
	}
	if check == "provider-models" {
		t.Fatalf("%q: row %q lists a provider that has no credential", args, line)
	}
}
