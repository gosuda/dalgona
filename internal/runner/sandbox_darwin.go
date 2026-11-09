//go:build darwin

package runner

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

const sandboxExec = "/usr/bin/sandbox-exec"

func Available() bool {
	info, err := os.Stat(sandboxExec)
	return err == nil && !info.IsDir() && info.Mode()&0o111 != 0
}

func configureSandbox(cmd *exec.Cmd, dir string, sandbox Sandbox) error {
	if sandbox.Mode != SandboxWorkspaceWrite || !Available() {
		return nil
	}
	roots, err := resolveRoots(sandbox.WritableRoots)
	if err != nil {
		return err
	}
	args := []string{sandboxExec, "-p", sandboxProfile(roots), "--"}
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

func sandboxProfile(roots []string) string {
	var profile strings.Builder
	profile.WriteString("(version 1) (allow default) (deny file-write*)")
	for _, root := range roots {
		profile.WriteString(` (allow file-write* (subpath "`)
		profile.WriteString(escapeSandboxRoot(root))
		profile.WriteString(`"))`)
	}
	profile.WriteString(` (allow file-write* (literal "/dev/null") (literal "/dev/zero") (regex #"^/dev/tty") (regex #"^/dev/fd/"))`)
	return profile.String()
}

func escapeSandboxRoot(root string) string {
	return strings.NewReplacer(`\`, `\\`, `"`, `\"`).Replace(root)
}

func syncFilesystem(string) error { return nil }
