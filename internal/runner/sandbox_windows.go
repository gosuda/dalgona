//go:build windows

package runner

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
	"syscall"

	"golang.org/x/sys/windows"
)

const probeCapabilitySID = "S-1-5-21-1-2-3-4"

var probeOnce = sync.OnceValue(probeRestrictedToken)

func UnavailableCause() string {
	if err := probeOnce(); err != nil {
		return "windows restricted token is unavailable: " + err.Error()
	}
	return ""
}

func probeRestrictedToken() error {
	sid, err := windows.StringToSid(probeCapabilitySID)
	if err != nil {
		return err
	}
	token, err := createRestrictedToken([]*windows.SID{sid})
	if err != nil {
		return err
	}
	defer func() { _ = token.Close() }()
	cmd := exec.Command("cmd", "/C", "ver")
	cmd.SysProcAttr = &syscall.SysProcAttr{Token: syscall.Token(token), CreationFlags: windows.CREATE_NO_WINDOW}
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("restricted child did not run: %w", err)
	}
	return nil
}

func configureSandbox(cmd *exec.Cmd, _ string, sandbox Sandbox) error {
	if sandbox.Mode == SandboxOff || sandbox.Mode == "" {
		return nil
	}
	if UnavailableCause() != "" {
		return nil
	}
	if len(sandbox.DeniedSockets) != 0 {
		return fmt.Errorf("runner: Windows network confinement is unavailable")
	}
	roots, err := windowsWritableRoots(cmd.Dir, sandbox)
	if err != nil {
		return err
	}
	return confineWindowsCommand(cmd, roots)
}

func windowsWritableRoots(cwd string, sandbox Sandbox) ([]string, error) {
	switch sandbox.Mode {
	case SandboxWorkspaceWrite:
		if cwd == "" {
			var err error
			cwd, err = os.Getwd()
			if err != nil {
				return nil, fmt.Errorf("runner: resolve command directory: %w", err)
			}
		}
		input := make([]string, 0, len(sandbox.WritableRoots)+1)
		input = append(input, cwd)
		input = append(input, sandbox.WritableRoots...)
		return resolveRoots(input)
	case SandboxOff, SandboxMode("read-only"):
		return nil, nil
	default:
		return nil, fmt.Errorf("runner: unsupported Windows sandbox mode %q", sandbox.Mode)
	}
}

func confineWindowsCommand(cmd *exec.Cmd, roots []string) error {
	seed, err := capabilitySeed()
	if err != nil {
		return err
	}
	readOnly, err := capabilitySID(seed, readOnlyCapabilityKey)
	if err != nil {
		return err
	}
	grants, err := rootCapabilities(seed, roots)
	if err != nil {
		return err
	}
	if err = grantRoots(grants); err != nil {
		return err
	}
	token, err := createRestrictedToken(restrictingSIDs(readOnly, grants))
	if err != nil {
		return err
	}
	if cmd.SysProcAttr == nil {
		cmd.SysProcAttr = &syscall.SysProcAttr{}
	}
	cmd.SysProcAttr.Token = syscall.Token(token)
	return nil
}

func releaseCommandToken(cmd *exec.Cmd) {
	if cmd.SysProcAttr == nil || cmd.SysProcAttr.Token == 0 {
		return
	}
	_ = windows.Token(cmd.SysProcAttr.Token).Close()
	cmd.SysProcAttr.Token = 0
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
	seen := make(map[string]struct{}, len(paths))
	for _, path := range paths {
		if path == "" {
			continue
		}
		resolved, err := filepath.EvalSymlinks(path)
		if err != nil {
			return nil, fmt.Errorf("resolve writable root %s: %w", path, err)
		}
		key := capabilityPathKey(resolved)
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		roots = append(roots, resolved)
	}
	return roots, nil
}

func execSandboxed(dir string, argv []string) error {
	return fmt.Errorf("runner: nested sandbox exec is unsupported")
}

func syncFilesystem(string) error { return nil }
