//go:build !windows

package runner

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"syscall"
)

func startDetached(ctx context.Context, self string, args []string, spec ProcessSpec) (*exec.Cmd, error) {
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("runner: detached start: %w", err)
	}
	cmd := exec.Command(self, args...)
	cmd.Dir = spec.Dir
	cmd.Env = spec.Env
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("runner: start shim: %w", err)
	}
	return cmd, nil
}

func signalProcess(shimPID int, pgid int, signal Signal) error {
	err := syscall.Kill(-pgid, unixSignal(signal))
	if err != nil && !errors.Is(err, syscall.ESRCH) {
		return fmt.Errorf("runner: signal process group: %w", err)
	}
	return nil
}

func unixSignal(signal Signal) syscall.Signal {
	if signal == SignalKill {
		return syscall.SIGKILL
	}
	return syscall.SIGTERM
}

func canceledExit(signal Signal) (int, string) {
	return -1, unixSignal(signal).String()
}

func configureProcessGroup(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
}

func processIsAlive(pid int) bool {
	return syscall.Kill(pid, 0) == nil
}

func assignJob(cmd *exec.Cmd) (func(), error) {
	return func() {}, nil
}

func exitDetails(waitErr error) (int, string) {
	if waitErr == nil {
		return 0, ""
	}
	exitErr, ok := errors.AsType[*exec.ExitError](waitErr)
	if !ok {
		return 1, ""
	}
	status, ok := exitErr.Sys().(syscall.WaitStatus)
	if ok && status.Signaled() {
		return exitErr.ExitCode(), status.Signal().String()
	}
	return exitErr.ExitCode(), ""
}

func syncDir(path string) error {
	dir, err := os.Open(path)
	if err != nil {
		return fmt.Errorf("open directory: %w", err)
	}
	syncErr := dir.Sync()
	closeErr := dir.Close()
	return errors.Join(syncErr, closeErr)
}
