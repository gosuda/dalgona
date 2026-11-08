//go:build linux

package runner

import (
	"os"
	"path/filepath"
	"testing"
)

func TestRemote_ignoresHarnessSandbox(t *testing.T) {
	if !Available() {
		t.Skip("Landlock ABI is 0")
	}
	dir := t.TempDir()
	workspace := filepath.Join(dir, "workspace")
	home := filepath.Join(dir, "home")
	for _, path := range []string{workspace, home} {
		if err := os.Mkdir(path, 0o700); err != nil {
			t.Fatal(err)
		}
	}
	remote := newPipeContractRunner(t)
	t.Cleanup(func() {
		if err := remote.Close(); err != nil {
			t.Error(err)
		}
	})
	env := setEnvironment(os.Environ(), "HOME", home)
	env = setEnvironment(env, "TMPDIR", filepath.Join(dir, "tmp"))
	env = setEnvironment(env, "XDG_CACHE_HOME", filepath.Join(dir, "cache"))
	spec := ProcessSpec{ID: "remote-sandbox", Argv: []string{"sh", "-c", `touch "$HOME/outside-remote"`}, Dir: workspace, Env: env, Sandbox: Sandbox{Mode: SandboxWorkspaceWrite, WritableRoots: []string{workspace}}}
	if err := remote.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	state := readContractOutput(t, remote, spec.ID)
	if state.Exit.Code != 0 {
		t.Fatalf("remote command exit = %d, output %s", state.Exit.Code, state.Data)
	}
	if _, err := os.Stat(filepath.Join(home, "outside-remote")); err != nil {
		t.Fatalf("remote write outside the workspace failed: %v", err)
	}
}
