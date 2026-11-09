//go:build !windows

package daemon

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"syscall"
)

func serviceUID() string { return strconv.Itoa(os.Getuid()) }
func spawn(home, exe string) error {
	logFile, err := os.OpenFile(home+"/daemon.log", os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o600)
	if err != nil {
		return fmt.Errorf("open daemon log: %w", err)
	}
	cmd := exec.Command(exe, "daemon")
	cmd.Stdout, cmd.Stderr = logFile, logFile
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
	if err := cmd.Start(); err != nil {
		return errors.Join(fmt.Errorf("start daemon: %w", err), logFile.Close())
	}
	if err := cmd.Process.Release(); err != nil {
		killErr := cmd.Process.Kill()
		_, waitErr := cmd.Process.Wait()
		return errors.Join(fmt.Errorf("release daemon: %w", err), killErr, waitErr, logFile.Close())
	}
	if err := logFile.Close(); err != nil {
		return fmt.Errorf("close daemon log: %w", err)
	}
	return nil
}
