//go:build !linux && !darwin

package runner

import (
	"fmt"
	"os/exec"
	"runtime"
)

func UnavailableCause() string { return runtime.GOOS + " has no harness sandbox" }

func configureSandbox(cmd *exec.Cmd, dir string, sandbox Sandbox) error { return nil }

func execSandboxed(dir string, argv []string) error {
	return fmt.Errorf("runner: nested sandbox exec is unsupported")
}

func syncFilesystem(string) error { return nil }
