//go:build !linux && !darwin

package runner

import (
	"fmt"
	"os/exec"
)

func Available() bool { return false }

func configureSandbox(cmd *exec.Cmd, dir string, sandbox Sandbox) error { return nil }

func execSandboxed(dir string, argv []string) error {
	return fmt.Errorf("runner: nested sandbox exec is unsupported")
}

func syncFilesystem(string) error { return nil }
