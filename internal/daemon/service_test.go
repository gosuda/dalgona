package daemon

import (
	"context"
	"encoding/xml"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func nilRun(context.Context, string, ...string) error { return nil }

func installLinuxUnit(t *testing.T, exe, dalgonaHome string) string {
	t.Helper()
	home := shortHome(t)
	if err := installLinuxService(home, exe, dalgonaHome, nilRun); err != nil {
		t.Fatal(err)
	}
	unit, unitErr := os.ReadFile(filepath.Join(home, ".config", "systemd", "user", "dalgona.service"))
	if unitErr != nil {
		t.Fatal(unitErr)
	}
	return string(unit)
}

func installDarwinPlist(t *testing.T, exe, dalgonaHome string) []byte {
	t.Helper()
	home := shortHome(t)
	if err := installDarwinService(home, exe, dalgonaHome, nilRun); err != nil {
		t.Fatal(err)
	}
	data, readErr := os.ReadFile(filepath.Join(home, "Library", "LaunchAgents", "dev.dalgona.daemon.plist"))
	if readErr != nil {
		t.Fatal(readErr)
	}
	return data
}

func plistArguments(data []byte) ([]string, error) {
	var doc struct {
		Args []string `xml:"dict>array>string"`
	}
	if err := xml.Unmarshal(data, &doc); err != nil {
		return nil, err
	}
	return doc.Args, nil
}

func TestServiceInstallQuotesAndEscapesTheExecutablePath(t *testing.T) {
	unit := installLinuxUnit(t, "/Users/a b/go/bin/dalgona", "")
	want := `ExecStart="/Users/a b/go/bin/dalgona" daemon`
	if !strings.Contains(unit, want) {
		t.Fatalf("unit %q lacks %q", unit, want)
	}
	args, argsErr := plistArguments(installDarwinPlist(t, `/a & b/<go>/dalgona`, ""))
	if argsErr != nil {
		t.Fatalf("plist is not valid XML: %v", argsErr)
	}
	if len(args) != 2 || args[0] != `/a & b/<go>/dalgona` || args[1] != "daemon" {
		t.Fatalf("ProgramArguments = %q, want the escaped executable round-tripped", args)
	}
}

func TestServiceInstallPinsDALGONAHomeWhenSet(t *testing.T) {
	t.Run("linux", func(t *testing.T) {
		unit := installLinuxUnit(t, "/abs/dalgona", "/x y/home")
		want := `Environment="DALGONA_HOME=/x y/home"`
		if !strings.Contains(unit, want) {
			t.Fatalf("unit %q lacks %q", unit, want)
		}
		plain := installLinuxUnit(t, "/abs/dalgona", "")
		if strings.Contains(plain, "DALGONA_HOME") {
			t.Fatalf("unit pins DALGONA_HOME without the variable set: %q", plain)
		}
	})
	t.Run("darwin", func(t *testing.T) {
		plist := installDarwinPlist(t, "/abs/dalgona", "/x y/home")
		want := "<key>EnvironmentVariables</key><dict><key>DALGONA_HOME</key><string>/x y/home</string></dict>"
		if !strings.Contains(string(plist), want) {
			t.Fatalf("plist %q lacks %q", plist, want)
		}
		plain := installDarwinPlist(t, "/abs/dalgona", "")
		if strings.Contains(string(plain), "EnvironmentVariables") {
			t.Fatalf("plist pins EnvironmentVariables without the variable set: %q", plain)
		}
	})
}

func TestUninstallRemovesTheServiceFileWhenTheServiceIsNotLoaded(t *testing.T) {
	for _, tc := range []struct{ name, goos, notLoaded string }{
		{"linux", "linux", "Failed to disable unit: Unit file dalgona.service not found."},
		{"linux-legacy", "linux", "Failed to disable unit: No such file or directory"},
		{"darwin", "darwin", `Could not find service "dev.dalgona.daemon" in gui/501`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			home := shortHome(t)
			path := writeServiceStub(t, home, tc.goos)
			run := func(context.Context, string, ...string) error { return errors.New(tc.notLoaded) }
			uninstallErr := UninstallService(ServiceEnv{Home: home, GOOS: tc.goos, Run: run})
			if uninstallErr != nil {
				t.Fatalf("uninstall with not-loaded %q = %v", tc.notLoaded, uninstallErr)
			}
			if _, statErr := os.Stat(path); !os.IsNotExist(statErr) {
				t.Fatalf("service file remains: %v", statErr)
			}
		})
	}
}

func TestUninstallStillRemovesTheFileWhenDisableFailsForAnotherReason(t *testing.T) {
	home := shortHome(t)
	path := writeServiceStub(t, home, "linux")
	run := func(context.Context, string, ...string) error { return errors.New("systemctl: connection refused") }
	uninstallErr := UninstallService(ServiceEnv{Home: home, GOOS: "linux", Run: run})
	if uninstallErr == nil || !strings.Contains(uninstallErr.Error(), "connection refused") {
		t.Fatalf("uninstall error = %v, want the disable failure reported", uninstallErr)
	}
	if _, statErr := os.Stat(path); !os.IsNotExist(statErr) {
		t.Fatalf("service file remains: %v", statErr)
	}
}

func writeServiceStub(t *testing.T, home, goos string) string {
	t.Helper()
	path := filepath.Join(home, ".config", "systemd", "user", "dalgona.service")
	if goos == "darwin" {
		path = filepath.Join(home, "Library", "LaunchAgents", "dev.dalgona.daemon.plist")
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte("stub"), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestSystemdUnitEscapesQuotesAndBackslashes(t *testing.T) {
	unit := installLinuxUnit(t, `/opt/a"b\c/dalgona`, `/home/x"y\z`)
	if want := `ExecStart="/opt/a\"b\\c/dalgona" daemon`; !strings.Contains(unit, want) {
		t.Fatalf("unit %q lacks escaped ExecStart %q", unit, want)
	}
	if want := `Environment="DALGONA_HOME=/home/x\"y\\z"`; !strings.Contains(unit, want) {
		t.Fatalf("unit %q lacks escaped Environment %q", unit, want)
	}
}

func TestSystemdUnitRejectsControlCharacters(t *testing.T) {
	for _, tc := range []struct{ name, exe, home string }{
		{"exe-newline", "/opt/bad\n/dalgona", ""},
		{"exe-cr", "/opt/bad\r/dalgona", ""},
		{"exe-control", "/opt/bad\x01/dalgona", ""},
		{"exe-nel", "/opt/bad\u0085/dalgona", ""},
		{"home-newline", "/abs/dalgona", "/home/bad\nname"},
		{"home-control", "/abs/dalgona", "/home/bad\x7fname"},
		{"home-nel", "/abs/dalgona", "/home/bad\u0085name"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			home := shortHome(t)
			if err := installLinuxService(home, tc.exe, tc.home, nilRun); err == nil {
				t.Fatalf("installLinuxService(%q, %q) = nil, want control-character error", tc.exe, tc.home)
			}
		})
	}
}

func TestWindowsUninstallToleratesMissingTask(t *testing.T) {
	for _, msg := range []string{
		"ERROR: The system cannot find the file specified.",
		`ERROR: The specified task "dalgona" does not exist.`,
	} {
		t.Run(msg, func(t *testing.T) {
			run := func(context.Context, string, ...string) error { return errors.New(msg) }
			if err := UninstallService(ServiceEnv{GOOS: "windows", Run: run}); err != nil {
				t.Fatalf("windows uninstall with missing task %q = %v, want nil", msg, err)
			}
		})
	}
}

func TestWindowsUninstallReportsMissingExecutable(t *testing.T) {
	run := func(context.Context, string, ...string) error {
		return errors.New(`exec: "schtasks": executable file not found in %PATH%`)
	}
	if err := UninstallService(ServiceEnv{GOOS: "windows", Run: run}); err == nil {
		t.Fatalf("windows uninstall with missing schtasks = nil, want error")
	}
}
