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

func lockGateBounded(path string, d time.Duration) (*os.File, error) {
	file, err := openLockFile(path)
	if err != nil {
		return nil, err
	}
	deadline := time.Now().Add(d)
	for {
		locked, lerr := tryLock(file)
		if lerr != nil {
			return nil, errors.Join(lerr, file.Close())
		}
		if locked {
			return file, nil
		}
		if !time.Now().Before(deadline) {
			return nil, errors.Join(errMirrorBusy, file.Close())
		}
		time.Sleep(time.Millisecond)
	}
}

func releaseGate(file *os.File) error {
	return releaseLock(file)
}

func isLockBusy(err error) bool {
	return errors.Is(err, unix.EWOULDBLOCK) || errors.Is(err, unix.EAGAIN)
}
