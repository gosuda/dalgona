//go:build linux

package runner

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"

	"github.com/landlock-lsm/go-landlock/landlock"
	landsyscall "github.com/landlock-lsm/go-landlock/landlock/syscall"
	"golang.org/x/sys/unix"
)

func Available() bool {
	version, err := landsyscall.LandlockGetABIVersion()
	return err == nil && version > 0
}

func configureSandbox(cmd *exec.Cmd, dir string, sandbox Sandbox) error {
	if sandbox.Mode != SandboxWorkspaceWrite || !Available() {
		return nil
	}
	self, err := selfExe()
	if err != nil {
		return fmt.Errorf("resolve shim executable: %w", err)
	}
	args := []string{self, "__shim", "--dir", dir, "--"}
	args = append(args, cmd.Args...)
	cmd.Path = self
	cmd.Args = args
	cmd.Env = setEnvironment(cmd.Env, shimExecEnvironment, "1")
	return nil
}

func execSandboxed(dir string, argv []string) error {
	if err := applySandbox(currentSandbox()); err != nil {
		return fmt.Errorf("apply sandbox: %w", err)
	}
	env := stripDeniedEnvironment(removeEnvironment(os.Environ(), shimExecEnvironment))
	if err := unix.Exec(argv[0], argv, env); err != nil {
		return fmt.Errorf("exec command: %w", err)
	}
	return nil
}

func applySandbox(sandbox Sandbox) error {
	if sandbox.Mode != SandboxWorkspaceWrite || !Available() {
		return nil
	}
	roots, err := resolveRoots(sandbox.WritableRoots)
	if err != nil {
		return err
	}
	if err := landlock.V3.BestEffort().RestrictPaths(landlock.RODirs("/"), landlock.RWDirs(roots...), landlock.RWDirs("/dev")); err != nil {
		return fmt.Errorf("restrict paths: %w", err)
	}
	return nil
}

func resolveRoots(input []string) ([]string, error) {
	cache, err := os.UserCacheDir()
	if err != nil {
		return nil, fmt.Errorf("resolve user cache: %w", err)
	}
	if err := os.MkdirAll(cache, 0o700); err != nil {
		return nil, fmt.Errorf("create user cache: %w", err)
	}
	paths := make([]string, 0, len(input)+2)
	paths = append(paths, input...)
	paths = append(paths, os.TempDir(), cache)
	roots := make([]string, 0, len(paths))
	for _, path := range paths {
		if path == "" {
			continue
		}
		resolved, err := filepath.EvalSymlinks(path)
		if err != nil {
			return nil, fmt.Errorf("resolve writable root %s: %w", path, err)
		}
		roots = append(roots, resolved)
	}
	return roots, nil
}

func syncFilesystem(path string) error {
	fd, err := unix.Open(path, unix.O_RDONLY|unix.O_DIRECTORY, 0)
	if err != nil {
		return fmt.Errorf("open syncfs directory: %w", err)
	}
	syncErr := unix.Syncfs(fd)
	closeErr := unix.Close(fd)
	return errors.Join(syncErr, closeErr)
}
