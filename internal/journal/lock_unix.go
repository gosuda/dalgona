//go:build !windows

package journal

import (
	"errors"
	"os"
	"time"

	"golang.org/x/sys/unix"
)

func flock(f *os.File, how int) error {
	for {
		err := unix.Flock(int(f.Fd()), how)
		if !errors.Is(err, unix.EINTR) {
			return err
		}
	}
}

func lockFile(f *os.File) error {
	return flock(f, unix.LOCK_EX|unix.LOCK_NB)
}

func waitFile(f *os.File) error {
	return flock(f, unix.LOCK_EX)
}

func waitFileShared(f *os.File) error {
	return flock(f, unix.LOCK_SH)
}

func unlockFile(f *os.File) error {
	return flock(f, unix.LOCK_UN)
}

func waitFileBounded(f *os.File, d time.Duration) error {
	deadline := time.Now().Add(d)
	for {
		locked, err := tryLock(f)
		if err != nil {
			return err
		}
		if locked {
			return nil
		}
		if !time.Now().Before(deadline) {
			return errMirrorBusy
		}
		time.Sleep(time.Millisecond)
	}
}

func isLockBusy(err error) bool {
	return errors.Is(err, unix.EWOULDBLOCK) || errors.Is(err, unix.EAGAIN)
}
