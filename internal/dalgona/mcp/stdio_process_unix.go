//go:build !windows

package mcp

import (
	"errors"
	"os"
	"os/exec"
	"syscall"
)

func configureStdioProcess(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	cmd.WaitDelay = stdioWaitDelay
}

func killStdioProcess(cmd *exec.Cmd) error {
	proc := cmd.Process
	if proc == nil {
		return nil
	}
	if err := syscall.Kill(-proc.Pid, syscall.SIGKILL); err != nil {
		if errors.Is(err, syscall.ESRCH) || errors.Is(err, os.ErrProcessDone) {
			return nil
		}
		if killErr := proc.Kill(); killErr != nil && !errors.Is(killErr, os.ErrProcessDone) {
			return killErr
		}
	}
	return nil
}

func assignStdioJob(_ *exec.Cmd) (func(), error) {
	return nil, nil
}
