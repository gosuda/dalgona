//go:build darwin

package runner

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"unicode"
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
	denied, err := resolveDeniedSockets(sandbox.DeniedSockets)
	if err != nil {
		return err
	}
	args := []string{sandboxExec, "-p", sandboxProfile(roots, denied), "--"}
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

func resolveDeniedSockets(input []string) ([]string, error) {
	denied := make([]string, 0, len(input)*3)
	for _, path := range input {
		if path == "" {
			continue
		}
		cleaned := filepath.Clean(path)
		resolved, err := filepath.EvalSymlinks(path)
		if err != nil {
			parent := filepath.Dir(cleaned)
			realParent, parentErr := filepath.EvalSymlinks(parent)
			if parentErr != nil {
				return nil, fmt.Errorf("resolve denied socket %s: %w", path, err)
			}
			resolved = filepath.Join(realParent, filepath.Base(cleaned))
		}
		canonical, canonErr := canonicalParentDir(resolved)
		if canonErr != nil {
			return nil, canonErr
		}
		var appendErr error
		denied, appendErr = appendDeniedSpellings(denied, cleaned, resolved, canonical)
		if appendErr != nil {
			return nil, appendErr
		}
	}
	sorted := slices.Clone(denied)
	slices.Sort(sorted)
	return slices.Compact(sorted), nil
}

func appendDeniedSpellings(denied []string, spellings ...string) ([]string, error) {
	for _, spelling := range spellings {
		if strings.IndexFunc(spelling, unicode.IsControl) >= 0 {
			return nil, fmt.Errorf("refuse to build a profile for a path with a control character: %q", spelling)
		}
		denied = append(denied, spelling)
	}
	return denied, nil
}

func canonicalParentDir(path string) (string, error) {
	realParent, err := filepath.EvalSymlinks(filepath.Dir(path))
	if err != nil {
		return "", fmt.Errorf("resolve denied socket parent %s: %w", path, err)
	}
	return filepath.Join(realParent, filepath.Base(path)), nil
}

func sandboxProfile(roots []string, denied []string) string {
	var profile strings.Builder
	profile.WriteString("(version 1) (allow default)")
	for _, socket := range denied {
		profile.WriteString(` (deny network-outbound (remote unix-socket (path-literal "`)
		profile.WriteString(escapeSandboxRoot(socket))
		profile.WriteString(`")))`)
	}
	profile.WriteString(" (deny file-write*)")
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
