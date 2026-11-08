//go:build linux

package conformance

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"syscall"
	"time"
)

func lockFile(path string, wait time.Duration) (func() error, error) {
	file, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open lock: %w", err)
	}
	deadline := time.Now().Add(wait)
	for {
		err = tryLock(file)
		if err == nil {
			return file.Close, nil
		}
		if !errors.Is(err, errLockBusy) || !time.Now().Before(deadline) {
			return nil, errors.Join(err, file.Close())
		}
		time.Sleep(100 * time.Millisecond)
	}
}

func tryLock(file *os.File) error {
	conn, err := file.SyscallConn()
	if err != nil {
		return fmt.Errorf("lock fd: %w", err)
	}
	var lockErr error
	if err := conn.Control(func(fd uintptr) { lockErr = syscall.Flock(int(fd), syscall.LOCK_EX|syscall.LOCK_NB) }); err != nil {
		return fmt.Errorf("lock fd: %w", err)
	}
	if errors.Is(lockErr, syscall.EWOULDBLOCK) {
		return errLockBusy
	}
	if lockErr != nil {
		return fmt.Errorf("flock: %w", lockErr)
	}
	return nil
}

func daemonPIDs(binary, home string) ([]int, error) {
	entries, err := os.ReadDir("/proc")
	if err != nil {
		return nil, fmt.Errorf("read /proc: %w", err)
	}
	resolved, err := filepath.EvalSymlinks(binary)
	if err != nil {
		resolved = binary
	}
	var pids []int
	for _, entry := range entries {
		pid, convErr := strconv.Atoi(entry.Name())
		if convErr != nil {
			continue
		}
		if isDaemon(pid, resolved, home) {
			pids = append(pids, pid)
		}
	}
	if len(pids) == 0 {
		return nil, errNoDaemon
	}
	return pids, nil
}

func isDaemon(pid int, binary, home string) bool {
	dir := "/proc/" + strconv.Itoa(pid)
	exe, err := os.Readlink(dir + "/exe")
	if err != nil || (exe != binary && exe != binary+" (deleted)") {
		return false
	}
	cmdline, err := os.ReadFile(dir + "/cmdline")
	if err != nil {
		return false
	}
	args := bytes.Split(bytes.TrimRight(cmdline, "\x00"), []byte{0})
	if len(args) != 2 || string(args[1]) != "daemon" {
		return false
	}
	environ, err := os.ReadFile(dir + "/environ")
	return err == nil && bytes.Contains(environ, []byte("DALGONA_HOME="+home+"\x00"))
}

const processTableAvailable = true

func signal(pid int, sig syscall.Signal) error {
	if err := syscall.Kill(pid, sig); err != nil && !errors.Is(err, syscall.ESRCH) {
		return fmt.Errorf("signal %d with %v: %w", pid, sig, err)
	}
	return nil
}

func terminate(pid int) error { return signal(pid, syscall.SIGTERM) }

func kill(pid int) error { return signal(pid, syscall.SIGKILL) }

func waitPID(pid int, deadline time.Time) bool {
	for time.Now().Before(deadline) {
		if gone(pid) {
			return true
		}
		time.Sleep(20 * time.Millisecond)
	}
	return gone(pid)
}

func gone(pid int) bool {
	stat, err := os.ReadFile("/proc/" + strconv.Itoa(pid) + "/stat")
	if errors.Is(err, os.ErrNotExist) {
		return true
	}
	if err != nil {
		return false
	}
	end := bytes.LastIndexByte(stat, ')')
	return end >= 0 && end+2 < len(stat) && stat[end+2] == 'Z'
}
