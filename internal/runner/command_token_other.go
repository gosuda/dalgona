//go:build !windows

package runner

import "os/exec"

func releaseCommandToken(*exec.Cmd) {}
