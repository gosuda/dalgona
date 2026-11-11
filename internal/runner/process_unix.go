//go:build !windows

package runner

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"syscall"
	"time"
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

const groupExitSettle = 2 * time.Second

func signalProcess(shimPID int, pgid int, signal Signal) error {
	deadline := time.Now().Add(groupExitSettle)
	for {
		killErr := syscall.Kill(-pgid, unixSignal(signal))
		if killErr == nil || errors.Is(killErr, syscall.ESRCH) {
			return nil
		}
		if !errors.Is(killErr, syscall.EPERM) {
			return fmt.Errorf("runner: signal process group: %w", killErr)
		}
		if processGroupDrained(pgid) {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("runner: signal process group: %w", killErr)
		}
		time.Sleep(5 * time.Millisecond)
	}
}

func processGroupGone(pgid int) bool {
	probeErr := syscall.Kill(-pgid, 0)
	if probeErr == nil {
		return false
	}
	if errors.Is(probeErr, syscall.ESRCH) {
		return true
	}
	return errors.Is(probeErr, syscall.EPERM) && processGroupDrained(pgid)
}

func processGroupStopped(pgid int) bool {
	return processGroupGone(pgid) || processGroupDrained(pgid)
}

func ensureProcessGroupGone(pgid int, signal Signal) {
	if pgid <= 0 {
		return
	}
	deadline := time.Now().Add(5 * time.Second)
	for {
		if processGroupStopped(pgid) {
			return
		}
		if time.Now().After(deadline) {
			return
		}
		_ = signalProcess(os.Getpid(), pgid, signal)
		time.Sleep(50 * time.Millisecond)
	}
}

func stopSpawnGroup(string, int) bool { return false }

func killSpawnGroup(shimPID int, pgid int) bool {
	_ = signalProcess(shimPID, pgid, SignalKill)
	ensureProcessGroupGone(pgid, SignalKill)
	return processGroupStopped(pgid)
}

func watchCancelWhileWaiting(dir string, pgid int, done <-chan struct{}) {
	if pgid <= 0 {
		return
	}
	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-done:
			return
		case <-ticker.C:
			sig, ok := readCancel(dir)
			if !ok {
				continue
			}
			if processGroupGone(pgid) {
				return
			}
			_ = signalProcess(os.Getpid(), pgid, sig)
		}
	}
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

func configureShimCommand(_ *exec.Cmd, _ []string) {}

func runWindowsShimHelper(_ string) int { return 2 }

func processIsAlive(pid int) bool {
	return syscall.Kill(pid, 0) == nil
}

func assignJob(cmd *exec.Cmd) (func(), error) {
	return func() {}, nil
}

func resumeCommand(_ *exec.Cmd) error { return nil }

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
