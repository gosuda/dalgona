//go:build linux

package runner

import (
	"errors"
	"fmt"

	"golang.org/x/sys/unix"
)

func waitProcessExit(pid int, start string) error {
	fd, err := unix.PidfdOpen(pid, 0)
	if errors.Is(err, unix.ESRCH) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("pidfd_open %d: %w", pid, err)
	}
	defer func() { _ = unix.Close(fd) }()
	if processStartTime(pid) != start {
		return nil
	}
	var info unix.Siginfo
	for {
		err := unix.Waitid(unix.P_PIDFD, fd, &info, unix.WEXITED|unix.WNOWAIT, nil)
		if errors.Is(err, unix.EINTR) {
			continue
		}
		if err == nil || errors.Is(err, unix.ECHILD) {
			return nil
		}
		return fmt.Errorf("waitid %d: %w", pid, err)
	}
}
