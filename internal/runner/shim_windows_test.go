//go:build windows

package runner

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
)

const (
	windowsShimHelperEnv  = "DALGONA_WINDOWS_SHIM_TEST_HELPER"
	windowsShimPidFileEnv = "DALGONA_WINDOWS_SHIM_TEST_PIDFILE"
	windowsShimMarker     = "windows-shim-marker"
)

func TestWindowsShimPassesQuotedArgumentVerbatim(t *testing.T) {
	dir := t.TempDir()
	if err := runShim(dir, WindowsShellArgv(`echo "a b"`)); err != nil {
		t.Fatal(err)
	}
	out, exit := readWindowsShimResult(t, dir)
	if !strings.Contains(out, `"a b"`) {
		t.Fatalf("output missing quoted text: %q", out)
	}
	if strings.Contains(out, `\`) {
		t.Fatalf("output carries escape backslashes: %q", out)
	}
	if !strings.Contains(exit, `"code":0`) {
		t.Fatalf("exit record is not success: %s", exit)
	}
}

func TestWindowsShimRunsQuotedPathWithSpace(t *testing.T) {
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	source, err := os.ReadFile(self)
	if err != nil {
		t.Fatal(err)
	}
	targetDir := filepath.Join(t.TempDir(), "dir with space")
	if err := os.MkdirAll(targetDir, 0o755); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(targetDir, "helper.exe")
	if err := os.WriteFile(target, source, 0o755); err != nil {
		t.Fatal(err)
	}
	t.Setenv(windowsShimHelperEnv, "marker")
	dir := t.TempDir()
	if err := runShim(dir, WindowsShellArgv(`"`+target+`"`)); err != nil {
		t.Fatal(err)
	}
	out, exit := readWindowsShimResult(t, dir)
	if !strings.Contains(out, windowsShimMarker) {
		t.Fatalf("output missing marker: %q", out)
	}
	if !strings.Contains(exit, `"code":0`) {
		t.Fatalf("exit record is not success: %s", exit)
	}
}

func runWindowsShimHelper(mode string) int {
	switch mode {
	case "hold":
		return runWindowsHoldGrandchild()
	case "marker":
		_, _ = fmt.Fprint(os.Stdout, windowsShimMarker)
		return 0
	default:
		return 2
	}
}

func runWindowsHoldGrandchild() int {
	pidFile := os.Getenv(windowsShimPidFileEnv)
	if pidFile == "" {
		return 2
	}
	ping, err := exec.LookPath("ping")
	if err != nil {
		return 1
	}
	child := exec.Command(ping, "-n", "60", "127.0.0.1")
	child.Stdout = os.Stdout
	if err := child.Start(); err != nil {
		return 1
	}
	if err := os.WriteFile(pidFile, []byte(strconv.Itoa(child.Process.Pid)), 0o600); err != nil {
		return 1
	}
	_ = child.Wait()
	return 0
}

func readWindowsShimResult(t *testing.T, dir string) (string, string) {
	t.Helper()
	out, err := os.ReadFile(filepath.Join(dir, "out"))
	if err != nil {
		t.Fatal(err)
	}
	exit, err := os.ReadFile(filepath.Join(dir, "exit"))
	if err != nil {
		t.Fatal(err)
	}
	return string(out), string(exit)
}
