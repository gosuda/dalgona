package codexapp

import (
	"bytes"
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

const desktopSourceText = "[Desktop Entry]\nName=ChatGPT\nExec=chatgpt --open\n"

type linuxFixture struct {
	env             Env
	home            string
	exe             string
	source          string
	environmentPath string
	userDesktop     string
	vscode          string
}

func newLinuxFixture(t *testing.T) linuxFixture {
	t.Helper()
	home := t.TempDir()
	source := filepath.Join(home, "system.desktop")
	return linuxFixture{
		env: Env{
			Home:           home,
			DalgonaHome:    filepath.Join(home, ".dalgona"),
			GOOS:           "linux",
			DesktopFile:    source,
			VSCodeSettings: filepath.Join(home, "settings.json"),
		},
		home:            home,
		exe:             filepath.Join(home, "bin", "dalgona"),
		source:          source,
		environmentPath: filepath.Join(home, ".config", "environment.d", "60-dalgona-codex.conf"),
		userDesktop:     filepath.Join(home, ".local", "share", "applications", "chatgpt.desktop"),
		vscode:          filepath.Join(home, "settings.json"),
	}
}

func writeTestFile(t *testing.T, path string, data []byte) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatalf("create parent for %s: %v", path, err)
	}
	if err := os.WriteFile(path, data, 0o600); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func assertFileBytes(t *testing.T, path string, want []byte) {
	t.Helper()
	got, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	if !bytes.Equal(got, want) {
		t.Fatalf("%s bytes:\n got %q\nwant %q", path, got, want)
	}
}

func assertMissing(t *testing.T, path string) {
	t.Helper()
	_, err := os.Stat(path)
	if !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("%s still exists or stat failed: %v", path, err)
	}
}

func TestInstallLinuxEnvironmentAndDesktop(t *testing.T) {
	t.Parallel()
	fixture := newLinuxFixture(t)
	writeTestFile(t, fixture.source, []byte(desktopSourceText))

	if err := Install(fixture.env, fixture.exe); err != nil {
		t.Fatalf("Install: %v", err)
	}

	assertFileBytes(t, fixture.environmentPath, []byte("CODEX_CLI_PATH="+fixture.exe+"\n"))
	wantDesktop := strings.Replace(desktopSourceText, "Exec=", "Exec=env CODEX_CLI_PATH="+fixture.exe+" ", 1)
	assertFileBytes(t, fixture.userDesktop, []byte(wantDesktop))
	assertOwnerOnly(t, recordPath(fixture.env.DalgonaHome))
}

func TestInstallVSCodeExistingKeyPreservesOtherBytes(t *testing.T) {
	t.Parallel()
	fixture := newLinuxFixture(t)
	original := []byte("{\n  // keep this comment\n  \"chatgpt.cliExecutable\": \"old\",\n  \"other\": true\n}\n")
	writeTestFile(t, fixture.vscode, original)
	want := bytes.Replace(original, []byte("\"old\""), []byte(strconv.Quote(fixture.exe)), 1)

	if err := Install(fixture.env, fixture.exe); err != nil {
		t.Fatalf("Install: %v", err)
	}

	assertFileBytes(t, fixture.vscode, want)
}

func TestVSCodeInsertAfterFirstBrace(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		src  string
		want string
	}{
		{
			name: "existing setting",
			src:  "{\n  \"other\": true\n}\n",
			want: "{\n  \"chatgpt.cliExecutable\": \"/x/dalgona\",\n  \"other\": true\n}\n",
		},
		{
			name: "empty object",
			src:  "{}\n",
			want: "{\n  \"chatgpt.cliExecutable\": \"/x/dalgona\"}\n",
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			got := rewriteVSCode([]byte(test.src), "/x/dalgona")
			if string(got) != test.want {
				t.Fatalf("rewriteVSCode:\n got %q\nwant %q", got, test.want)
			}
		})
	}
}

func TestUninstallRestoresFilesByteExactly(t *testing.T) {
	t.Parallel()
	fixture := newLinuxFixture(t)
	writeTestFile(t, fixture.source, []byte(desktopSourceText))
	originalEnvironment := []byte("OLD=value\r\n")
	originalDesktop := []byte("user desktop bytes\r\n")
	originalVSCode := []byte("{\n  // comment\n  \"chatgpt.cliExecutable\": \"old\"\n}\n")
	writeTestFile(t, fixture.environmentPath, originalEnvironment)
	writeTestFile(t, fixture.userDesktop, originalDesktop)
	writeTestFile(t, fixture.vscode, originalVSCode)

	if err := Install(fixture.env, fixture.exe); err != nil {
		t.Fatalf("Install: %v", err)
	}
	if err := Uninstall(fixture.env); err != nil {
		t.Fatalf("Uninstall: %v", err)
	}

	assertFileBytes(t, fixture.environmentPath, originalEnvironment)
	assertFileBytes(t, fixture.userDesktop, originalDesktop)
	assertFileBytes(t, fixture.vscode, originalVSCode)
	assertMissing(t, recordPath(fixture.env.DalgonaHome))
}

func TestUninstallDeletesOnlyCreatedFiles(t *testing.T) {
	t.Parallel()
	fixture := newLinuxFixture(t)
	writeTestFile(t, fixture.source, []byte(desktopSourceText))
	unrelated := filepath.Join(fixture.home, ".config", "environment.d", "unrelated.conf")
	writeTestFile(t, unrelated, []byte("keep=true\n"))

	if err := Install(fixture.env, fixture.exe); err != nil {
		t.Fatalf("Install: %v", err)
	}
	if err := Uninstall(fixture.env); err != nil {
		t.Fatalf("Uninstall: %v", err)
	}

	assertMissing(t, fixture.environmentPath)
	assertMissing(t, fixture.userDesktop)
	assertMissing(t, fixture.vscode)
	assertMissing(t, recordPath(fixture.env.DalgonaHome))
	assertFileBytes(t, unrelated, []byte("keep=true\n"))
	assertFileBytes(t, fixture.source, []byte(desktopSourceText))
}

func TestStatusReportsMechanisms(t *testing.T) {
	t.Parallel()
	fixture := newLinuxFixture(t)
	writeTestFile(t, fixture.source, []byte(desktopSourceText))
	if err := Install(fixture.env, fixture.exe); err != nil {
		t.Fatalf("Install: %v", err)
	}

	status := Status(fixture.env, fixture.exe)
	wantNames := []string{"environment.d", "vscode", "desktop"}
	wantPaths := []string{fixture.environmentPath, fixture.vscode, fixture.userDesktop}
	assertStatusIdentity(t, status, wantNames, wantPaths)
	assertStatusMatch(t, status, true)

	mismatched := Status(fixture.env, filepath.Join(fixture.home, "other-dalgona"))
	assertStatusMatch(t, mismatched, false)
}

func assertStatusIdentity(t *testing.T, status []Mechanism, names, paths []string) {
	t.Helper()
	if len(status) != len(names) {
		t.Fatalf("Status returned %d mechanisms, want %d", len(status), len(names))
	}
	for i, mechanism := range status {
		if mechanism.Name != names[i] || mechanism.Path != paths[i] {
			t.Errorf("mechanism %d = %#v, want name %s path %s", i, mechanism, names[i], paths[i])
		}
	}
}

func assertStatusMatch(t *testing.T, status []Mechanism, want bool) {
	t.Helper()
	for _, mechanism := range status {
		if mechanism.Installed != want || mechanism.Matches != want {
			t.Errorf("mechanism %s match state = %#v, want %t", mechanism.Name, mechanism, want)
		}
		if want && mechanism.Current == "" {
			t.Errorf("mechanism %s has no current value: %#v", mechanism.Name, mechanism)
		}
	}
}

func TestInstallRejectsRelativeExecutable(t *testing.T) {
	t.Parallel()
	fixture := newLinuxFixture(t)

	err := Install(fixture.env, "dalgona")
	if err == nil || err.Error() != "codexapp: executable path must be absolute" {
		t.Fatalf("Install error = %v", err)
	}
	assertMissing(t, fixture.environmentPath)
	assertMissing(t, fixture.vscode)
	assertMissing(t, recordPath(fixture.env.DalgonaHome))
}

func TestInstallUnreadableDesktopChangesNothing(t *testing.T) {
	t.Parallel()
	fixture := newLinuxFixture(t)
	if err := os.Mkdir(fixture.source, 0o700); err != nil {
		t.Fatalf("create desktop directory: %v", err)
	}

	if err := Install(fixture.env, fixture.exe); err == nil {
		t.Fatal("Install succeeded with an unreadable desktop source")
	}
	assertMissing(t, fixture.environmentPath)
	assertMissing(t, fixture.vscode)
	assertMissing(t, recordPath(fixture.env.DalgonaHome))
}

func TestReinstallRecordsNewlyActiveMechanism(t *testing.T) {
	t.Parallel()
	fixture := newLinuxFixture(t)
	if err := Install(fixture.env, fixture.exe); err != nil {
		t.Fatalf("first Install: %v", err)
	}
	assertMissing(t, fixture.userDesktop)
	writeTestFile(t, fixture.source, []byte(desktopSourceText))
	if err := Install(fixture.env, fixture.exe); err != nil {
		t.Fatalf("second Install: %v", err)
	}
	if _, err := os.Stat(fixture.userDesktop); err != nil {
		t.Fatalf("desktop was not written by reinstall: %v", err)
	}
	if err := Uninstall(fixture.env); err != nil {
		t.Fatalf("Uninstall: %v", err)
	}
	assertMissing(t, fixture.userDesktop)
	assertMissing(t, fixture.environmentPath)
	assertMissing(t, fixture.vscode)
}
