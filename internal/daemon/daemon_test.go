package daemon

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	if os.Getenv(realDaemonEnv) != "" {
		os.Exit(runRealDaemon())
	}
	if home := os.Getenv("DALGONA_TEST_DAEMON"); home != "" {
		runDaemonTestHelper(home)
	}
	os.Exit(m.Run())
}

func runDaemonTestHelper(home string) {
	ln := startDaemonTestHelper(home)
	for {
		conn, err := ln.Accept()
		if err != nil {
			os.Exit(0)
		}
		if _, err := conn.Write([]byte("ok")); err != nil {
			slog.Error("daemon test helper write", "error", err)
		}
		if err := conn.Close(); err != nil {
			slog.Error("daemon test helper close", "error", err)
		}
	}
}

func startDaemonTestHelper(home string) net.Listener {
	if err := os.MkdirAll(home, 0o700); err != nil {
		daemonTestFailure(err)
	}
	pid := []byte(strconv.Itoa(os.Getpid()) + "\n")
	if err := os.WriteFile(filepath.Join(home, "helper.pid"), pid, 0o600); err != nil {
		daemonTestFailure(err)
	}
	ln, err := net.Listen("unix", filepath.Join(home, "dalgona.sock"))
	if err != nil {
		daemonTestFailure(err)
	}
	f, err := os.OpenFile(filepath.Join(home, "spawns"), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		daemonTestFailure(err)
	}
	if _, err := f.WriteString("1\n"); err != nil {
		daemonTestFailure(err)
	}
	if err := f.Close(); err != nil {
		daemonTestFailure(err)
	}
	return ln
}

func daemonTestFailure(err error) {
	slog.Error("daemon test helper failed", "error", err)
	os.Exit(2)
}

func TestEnsureStartsAndReuses(t *testing.T) {
	home := shortHome(t)
	t.Cleanup(func() { stopDaemonHelper(t, home) })
	t.Setenv("DALGONA_TEST_DAEMON", home)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	c, err := Ensure(ctx, home, exe)
	if err != nil {
		t.Fatal(err)
	}
	if closeErr := c.Close(); closeErr != nil {
		t.Fatal(closeErr)
	}
	cancel()
	c, err = Ensure(t.Context(), home, exe)
	if err != nil {
		t.Fatal(err)
	}
	if closeErr := c.Close(); closeErr != nil {
		t.Fatal(closeErr)
	}
	data, err := os.ReadFile(filepath.Join(home, "spawns"))
	if err != nil {
		t.Fatal(err)
	}
	if got := strings.Count(string(data), "1\n"); got != 1 {
		t.Fatalf("spawns=%d", got)
	}
}

func stopDaemonHelper(t *testing.T, home string) {
	t.Helper()
	pid, err := readHelperPID(home)
	if err != nil {
		t.Errorf("read helper pid: %v", err)
		return
	}
	process, err := os.FindProcess(pid)
	if err != nil {
		t.Errorf("find helper process: %v", err)
		return
	}
	if err := process.Kill(); err != nil && !errors.Is(err, os.ErrProcessDone) {
		t.Errorf("kill helper process: %v", err)
	}
	waitForDaemonHelperExit(t, home)
}

func waitForDaemonHelperExit(t *testing.T, home string) {
	t.Helper()
	socket := filepath.Join(home, "dalgona.sock")
	deadline := time.Now().Add(time.Second)
	for {
		conn, err := net.DialTimeout("unix", socket, 10*time.Millisecond)
		if err != nil {
			return
		}
		if err := conn.Close(); err != nil {
			t.Errorf("close helper probe: %v", err)
			return
		}
		if time.Now().After(deadline) {
			t.Error("helper process still accepts connections")
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func readHelperPID(home string) (int, error) {
	path := filepath.Join(home, "helper.pid")
	deadline := time.Now().Add(time.Second)
	for {
		data, err := os.ReadFile(path)
		if err == nil {
			pid, parseErr := strconv.Atoi(strings.TrimSpace(string(data)))
			if parseErr != nil {
				return 0, fmt.Errorf("parse pid: %w", parseErr)
			}
			return pid, nil
		}
		if !errors.Is(err, os.ErrNotExist) || time.Now().After(deadline) {
			return 0, fmt.Errorf("read %s: %w", path, err)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestEnsureTimeout(t *testing.T) {
	old := ensureTimeout
	ensureTimeout = 20 * time.Millisecond
	defer func() { ensureTimeout = old }()
	home := shortHome(t)
	exe := filepath.Join(home, "never")
	if err := os.WriteFile(exe, []byte("#!/bin/sh\nexit 0\n"), 0o700); err != nil {
		t.Fatal(err)
	}
	_, err := Ensure(t.Context(), home, exe)
	if err == nil || err.Error() != fmt.Sprintf("daemon did not start: see %s", filepath.Join(home, "daemon.log")) {
		t.Fatalf("err=%v", err)
	}
}

func TestServiceFilesAndCommands(t *testing.T) {
	for _, tc := range []struct {
		name, goos string
		check      func(string) bool
	}{
		{"linux", "linux", func(s string) bool { return strings.Contains(s, "Restart=always") }},
		{"darwin", "darwin", func(s string) bool { return strings.Contains(s, "RunAtLoad") && strings.Contains(s, "KeepAlive") }},
	} {
		t.Run(tc.name, func(t *testing.T) { testService(t, tc.goos, tc.check) })
	}
	calls := []string{}
	home := shortHome(t)
	run := func(_ context.Context, name string, args ...string) error {
		calls = append(calls, name+" "+strings.Join(args, " "))
		return nil
	}
	if err := InstallService(ServiceEnv{Home: home, GOOS: "windows", Run: run}, `C:\dalgona.exe`); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(calls[0], "/Create /SC ONLOGON /TN dalgona") {
		t.Fatalf("call=%s", calls[0])
	}
	if err := UninstallService(ServiceEnv{Home: home, GOOS: "windows", Run: run}); err != nil {
		t.Fatal(err)
	}
}

func testService(t *testing.T, goos string, check func(string) bool) {
	home := shortHome(t)
	calls := []string{}
	run := func(_ context.Context, name string, args ...string) error {
		calls = append(calls, name+" "+strings.Join(args, " "))
		return nil
	}
	if err := InstallService(ServiceEnv{Home: home, GOOS: goos, Run: run}, "/abs/dalgona"); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(home, ".config", "systemd", "user", "dalgona.service")
	installCommand := "systemctl --user enable --now dalgona.service"
	uninstallCommand := "systemctl --user disable --now dalgona.service"
	if goos == "darwin" {
		path = filepath.Join(home, "Library", "LaunchAgents", "dev.dalgona.daemon.plist")
		installCommand = "launchctl bootstrap gui/" + serviceUID() + " " + path
		uninstallCommand = "launchctl bootout gui/" + serviceUID() + " " + path
	}
	requireServiceCommand(t, calls, 0, installCommand)
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !check(string(data)) {
		t.Fatalf("bad service file")
	}
	if err := UninstallService(ServiceEnv{Home: home, GOOS: goos, Run: run}); err != nil {
		t.Fatal(err)
	}
	requireServiceCommand(t, calls, 1, uninstallCommand)
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatalf("service remains")
	}
}

func requireServiceCommand(t *testing.T, calls []string, index int, want string) {
	t.Helper()
	if len(calls) != index+1 || calls[index] != want {
		t.Fatalf("service calls=%v, want command %q at %d", calls, want, index)
	}
}
