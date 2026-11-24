//go:build darwin

package runner

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
)

const sandboxExec = "/usr/bin/sandbox-exec"

func UnavailableCause() string {
	info, err := os.Stat(sandboxExec)
	if err != nil || info.IsDir() || info.Mode()&0o111 == 0 {
		return "/usr/bin/sandbox-exec is missing"
	}
	return ""
}

func configureSandbox(cmd *exec.Cmd, dir string, sandbox Sandbox) error {
	if sandbox.Mode != SandboxWorkspaceWrite || UnavailableCause() != "" {
		return nil
	}
	roots, err := resolveRoots(sandbox.WritableRoots)
	if err != nil {
		return err
	}
	denied, err := deniedSocketSpellings(sandbox.DeniedSockets, filepath.EvalSymlinks)
	if err != nil {
		return err
	}
	args := []string{sandboxExec, "-p", seatbeltProfile(roots, denied), "--"}
	args = append(args, cmd.Args...)
	cmd.Path = sandboxExec
	cmd.Args = args
	return nil
}

func execSandboxed(dir string, argv []string) error {
	return fmt.Errorf("runner: unexpected nested shim on macOS")
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

func syncFilesystem(string) error { return nil }
