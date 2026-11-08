package daemon

import (
	"context"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"time"
)

type CommandRunner func(context.Context, string, ...string) error

type ServiceEnv struct {
	Home, GOOS, APPDATA string
	Run                 CommandRunner
}

var (
	ensureTimeout = 5 * time.Second
	ensureRetry   = 50 * time.Millisecond
)

func Ensure(ctx context.Context, home, exe string) (net.Conn, error) {
	if err := os.MkdirAll(home, 0o700); err != nil {
		return nil, fmt.Errorf("daemon home: %w", err)
	}
	sock := SocketPath(home)
	if conn, err := net.Dial("unix", sock); err == nil {
		return conn, nil
	}
	if err := spawn(home, exe); err != nil {
		return nil, err
	}
	deadline := time.Now().Add(ensureTimeout)
	for {
		if conn, err := net.DialTimeout("unix", sock, 200*time.Millisecond); err == nil {
			return conn, nil
		}
		if time.Now().After(deadline) {
			return nil, fmt.Errorf("daemon did not start: see %s", filepath.Join(home, "daemon.log"))
		}
		t := time.NewTimer(ensureRetry)
		select {
		case <-ctx.Done():
			t.Stop()
			return nil, context.Cause(ctx)
		case <-t.C:
		}
	}
}

func InstallService(env ServiceEnv, exe string) error {
	goos := env.GOOS
	if goos == "" {
		goos = runtime.GOOS
	}
	run := env.Run
	if run == nil {
		run = runCommand
	}
	switch goos {
	case "linux":
		return installLinuxService(env.Home, exe, run)
	case "darwin":
		return installDarwinService(env.Home, exe, run)
	case "windows":
		return installWindowsService(exe, run)
	default:
		return fmt.Errorf("unsupported operating system %q", goos)
	}
}

func installLinuxService(home, exe string, run CommandRunner) error {
	dir := filepath.Join(home, ".config", "systemd", "user")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("create systemd user directory: %w", err)
	}
	unit := fmt.Sprintf("[Unit]\nDescription=dalgona daemon\n[Service]\nExecStart=%s daemon\nRestart=always\n[Install]\nWantedBy=default.target\n", exe)
	if err := os.WriteFile(filepath.Join(dir, "dalgona.service"), []byte(unit), 0o600); err != nil {
		return fmt.Errorf("write systemd user unit: %w", err)
	}
	if err := run(context.Background(), "systemctl", "--user", "enable", "--now", "dalgona.service"); err != nil {
		return fmt.Errorf("enable dalgona service: %w", err)
	}
	return nil
}

func installDarwinService(home, exe string, run CommandRunner) error {
	dir := filepath.Join(home, "Library", "LaunchAgents")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return fmt.Errorf("create LaunchAgents directory: %w", err)
	}
	plist := fmt.Sprintf("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Label</key><string>dev.dalgona.daemon</string><key>ProgramArguments</key><array><string>%s</string><string>daemon</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/></dict></plist>\n", exe)
	path := filepath.Join(dir, "dev.dalgona.daemon.plist")
	if err := os.WriteFile(path, []byte(plist), 0o600); err != nil {
		return fmt.Errorf("write LaunchAgent: %w", err)
	}
	if err := run(context.Background(), "launchctl", "bootstrap", "gui/"+serviceUID(), path); err != nil {
		return fmt.Errorf("bootstrap dalgona LaunchAgent: %w", err)
	}
	return nil
}

func installWindowsService(exe string, run CommandRunner) error {
	if err := run(context.Background(), "schtasks", "/Create", "/SC", "ONLOGON", "/TN", "dalgona", "/TR", fmt.Sprintf("\"%s\" daemon", exe), "/F"); err != nil {
		return fmt.Errorf("create dalgona scheduled task: %w", err)
	}
	return nil
}

func UninstallService(env ServiceEnv) error {
	goos := env.GOOS
	if goos == "" {
		goos = runtime.GOOS
	}
	run := env.Run
	if run == nil {
		run = runCommand
	}
	switch goos {
	case "linux":
		if err := run(context.Background(), "systemctl", "--user", "disable", "--now", "dalgona.service"); err != nil {
			return err
		}
		return os.Remove(filepath.Join(env.Home, ".config", "systemd", "user", "dalgona.service"))
	case "darwin":
		path := filepath.Join(env.Home, "Library", "LaunchAgents", "dev.dalgona.daemon.plist")
		if err := run(context.Background(), "launchctl", "bootout", "gui/"+serviceUID(), path); err != nil {
			return err
		}
		return os.Remove(path)
	case "windows":
		return run(context.Background(), "schtasks", "/Delete", "/TN", "dalgona", "/F")
	default:
		return fmt.Errorf("unsupported operating system %q", goos)
	}
}
