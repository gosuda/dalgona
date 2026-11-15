//go:build windows

package runner

import "os/exec"

func configureShellCapture(*exec.Cmd) {}
