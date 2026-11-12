//go:build darwin

package runner

import (
	"errors"
	"fmt"

	"golang.org/x/sys/unix"
)

func waitProcessExit(pid int, start string) error {
	kq, err := unix.Kqueue()
	if err != nil {
		return fmt.Errorf("kqueue: %w", err)
	}
	defer func() { _ = unix.Close(kq) }()
	var change unix.Kevent_t
	unix.SetKevent(&change, pid, unix.EVFILT_PROC, unix.EV_ADD|unix.EV_ONESHOT)
	change.Fflags = unix.NOTE_EXIT
	if _, err := unix.Kevent(kq, []unix.Kevent_t{change}, nil, nil); err != nil {
		if errors.Is(err, unix.ESRCH) {
			return nil
		}
		return fmt.Errorf("kevent register %d: %w", pid, err)
	}
	if processStartTime(pid) != start {
		return nil
	}
	var event [1]unix.Kevent_t
	for {
		_, err := unix.Kevent(kq, nil, event[:], nil)
		if errors.Is(err, unix.EINTR) {
			continue
		}
		if err != nil {
			return fmt.Errorf("kevent wait %d: %w", pid, err)
		}
		return nil
	}
}
