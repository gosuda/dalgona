//go:build linux

package runner

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestLocal_linuxSandbox(t *testing.T) {
	if UnavailableCause() != "" {
		t.Skip("Landlock ABI is 0")
	}
	dir := t.TempDir()
	workspace := filepath.Join(dir, "workspace")
	home := filepath.Join(dir, "home")
	temp := filepath.Join(dir, "tmp")
	cache := filepath.Join(dir, "cache")
	for _, path := range []string{workspace, home, temp, cache} {
		if err := os.Mkdir(path, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	env := setEnvironment(os.Environ(), "HOME", home)
	env = setEnvironment(env, "TMPDIR", temp)
	env = setEnvironment(env, "XDG_CACHE_HOME", cache)
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	command := `if touch "$HOME/outside-runner"; then echo outside-ok; else echo outside-denied; fi; touch ./inside`
	spec := ProcessSpec{ID: "sandbox", Argv: []string{"sh", "-c", command}, Dir: workspace, Env: env, Sandbox: Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{workspace}}}
	startTestProcess(t, local, spec)
	state := waitForExit(t, local, "sandbox")
	if state.Exit.Code != 0 {
		t.Fatalf("sandbox command exit = %d, output %s", state.Exit.Code, state.Data)
	}
	if strings.Contains(string(state.Data), "outside-ok") || !strings.Contains(string(state.Data), "outside-denied") {
		t.Fatalf("unexpected sandbox output: %s", state.Data)
	}
	if _, err := os.Stat(filepath.Join(workspace, "inside")); err != nil {
		t.Fatalf("inside write failed: %v", err)
	}
	if _, err := os.Stat(filepath.Join(home, "outside-runner")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("outside write exists or stat failed unexpectedly: %v", err)
	}
}
