//go:build !windows

package runner

import (
	"os/exec"
	"syscall"
)

func configureShellCapture(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
}
