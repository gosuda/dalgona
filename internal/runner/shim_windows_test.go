//go:build windows

package runner

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"golang.org/x/sys/windows"
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
	out, _ := readWindowsShimResult(t, dir)
	if got := strings.TrimSpace(out); got != `"a b"` {
		t.Fatalf("output = %q, want %q", got, `"a b"`)
	}
	requireWindowsShimExit(t, dir, 0)
}

func TestWindowsShimRunsQuotedPathWithSpace(t *testing.T) {
	target := copySelfIntoSpacedDirectory(t)
	t.Setenv(windowsShimHelperEnv, "marker")
	dir := t.TempDir()
	if err := runShim(dir, WindowsShellArgv(`"`+target+`"`)); err != nil {
		t.Fatal(err)
	}
	out, _ := readWindowsShimResult(t, dir)
	if got := strings.TrimSpace(out); got != windowsShimMarker {
		t.Fatalf("output = %q, want %q", got, windowsShimMarker)
	}
	requireWindowsShimExit(t, dir, 0)
}

func copySelfIntoSpacedDirectory(t *testing.T) string {
	t.Helper()
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
	return target
}

func requireWindowsShimExit(t *testing.T, dir string, code int) {
	t.Helper()
	_, raw := readWindowsShimResult(t, dir)
	record := readWindowsShimExit(t, dir)
	if record == nil || record.Code != code || record.Signal != "" || record.EndedAt.IsZero() {
		t.Fatalf("exit record = %+v, want code %d with empty signal", record, code)
	}
	if want := fmt.Sprintf(`"code":%d`, code); !strings.Contains(raw, want) {
		t.Fatalf("exit record %s lacks %s", raw, want)
	}
}

func waitWindowsShimDone(t *testing.T, done <-chan error) {
	t.Helper()
	select {
	case err := <-done:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(60 * time.Second):
		t.Fatal("shim did not exit after job kill")
	}
}

func runWindowsShimHelper(mode string) int {
	switch mode {
	case "hold":
		return runWindowsHoldGrandchild()
	case "leak":
		return runWindowsLeakGrandchild()
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

func runWindowsLeakGrandchild() int {
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

func readWindowsShimExit(t *testing.T, dir string) *Exit {
	t.Helper()
	exit, err := readExit(filepath.Join(dir, "exit"))
	if err != nil {
		t.Fatalf("read shim exit: %v", err)
	}
	return exit
}

func TestWindowsShimKillsGrandchildThroughJob(t *testing.T) {
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	pidFile := filepath.Join(t.TempDir(), "grandchild.pid")
	t.Setenv(windowsShimHelperEnv, "hold")
	t.Setenv(windowsShimPidFileEnv, pidFile)
	t.Cleanup(func() {
		killWindowsProcessByFile(pidFile)
	})
	dir := t.TempDir()
	done := make(chan error, 1)
	go func() {
		done <- runShim(dir, []string{self})
	}()
	grandchild := waitWindowsShimPID(t, pidFile)
	shimPID, pgid := readWindowsShimIDs(t, dir)
	if err := signalProcess(shimPID, pgid, SignalKill); err != nil {
		t.Fatal(err)
	}
	waitWindowsShimDone(t, done)
	requireWindowsShimExit(t, dir, 137)
	waitWindowsProcessGone(t, grandchild)
}

func TestWindowsShimKillsGrandchildOnLeaderExit(t *testing.T) {
	self, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	pidFile := filepath.Join(t.TempDir(), "grandchild.pid")
	t.Setenv(windowsShimHelperEnv, "leak")
	t.Setenv(windowsShimPidFileEnv, pidFile)
	t.Cleanup(func() {
		killWindowsProcessByFile(pidFile)
	})
	dir := t.TempDir()
	done := make(chan error, 1)
	go func() {
		done <- runShim(dir, []string{self})
	}()
	grandchild := waitWindowsShimPID(t, pidFile)
	outsider := exec.Command("ping", "-n", "60", "127.0.0.1")
	if err := outsider.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = outsider.Process.Kill()
		_ = outsider.Wait()
	})
	waitWindowsShimDone(t, done)
	requireWindowsShimExit(t, dir, 0)
	waitWindowsProcessGone(t, grandchild)
	if windowsShimProcessGone(outsider.Process.Pid) {
		t.Fatal("a process outside the job object was killed")
	}
}

func TestWindowsShimFastExitRecordsZero(t *testing.T) {
	for i := range 20 {
		t.Run(strconv.Itoa(i), func(t *testing.T) {
			dir := t.TempDir()
			if err := runShim(dir, WindowsShellArgv("exit 0")); err != nil {
				t.Fatal(err)
			}
			requireWindowsShimExit(t, dir, 0)
		})
	}
}

func waitWindowsShimPID(t *testing.T, path string) int {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for {
		if pid, ok := readWindowsPIDFile(path); ok {
			return pid
		}
		if time.Now().After(deadline) {
			t.Fatalf("pid file %s did not appear", path)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func readWindowsShimIDs(t *testing.T, dir string) (int, int) {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for {
		shim, ok := readWindowsPIDFile(filepath.Join(dir, "pid"))
		group, groupOK := readWindowsPIDFile(filepath.Join(dir, "pgid"))
		if ok && groupOK {
			return shim, group
		}
		if time.Now().After(deadline) {
			t.Fatal("shim pid files did not appear")
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func readWindowsPIDFile(path string) (int, bool) {
	data, err := os.ReadFile(path)
	if err != nil {
		return 0, false
	}
	fields := strings.Fields(string(data))
	if len(fields) == 0 {
		return 0, false
	}
	pid, err := strconv.Atoi(fields[0])
	if err != nil {
		return 0, false
	}
	return pid, true
}

func waitWindowsProcessGone(t *testing.T, pid int) {
	t.Helper()
	deadline := time.Now().Add(30 * time.Second)
	for {
		if windowsShimProcessGone(pid) {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("grandchild %d still alive", pid)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func windowsShimProcessGone(pid int) bool {
	wpid, ok := windowsPID(pid)
	if !ok {
		return true
	}
	process, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, wpid)
	if err != nil {
		return errors.Is(err, windows.ERROR_INVALID_PARAMETER)
	}
	defer func() { _ = windows.CloseHandle(process) }()
	var code uint32
	if err := windows.GetExitCodeProcess(process, &code); err != nil {
		return false
	}
	return code != stillActive
}

func killWindowsProcessByFile(path string) {
	pid, ok := readWindowsPIDFile(path)
	if !ok {
		return
	}
	_ = terminateSpawnPID(pid)
}
