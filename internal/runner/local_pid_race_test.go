//go:build !windows

package runner

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLocalReadAfterStartReportsLive(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	env := append(os.Environ(), shimPauseEnvironment+"=pid:5s")
	spec := ProcessSpec{ID: "pid-race", Argv: []string{"sleep", "30"}, Dir: dir, Env: env, Sandbox: Sandbox{Mode: SandboxOff}}
	if err := local.Start(t.Context(), spec); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = local.Signal(t.Context(), "pid-race", SignalKill)
	})
	state, err := local.Read(t.Context(), "pid-race", 0)
	if err != nil {
		t.Fatal(err)
	}
	if state.Exit != nil {
		t.Fatalf("first Read reported exit %+v before startup completed", state.Exit)
	}
	if !state.Alive {
		t.Fatal("first Read after Start reported dead with no exit record")
	}
}
