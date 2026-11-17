//go:build !windows

package runner

import (
	"path/filepath"
	"testing"
)

func TestLocalReadAfterStartReportsLive(t *testing.T) {
	dir := t.TempDir()
	local := newTestLocal(t, filepath.Join(dir, "runs"))
	gate := startGatedProcess(t, local, ProcessSpec{ID: "pid-race", Argv: []string{"sleep", "30"}, Dir: dir, Sandbox: Sandbox{Mode: SandboxOff}}, "pid")
	gate.await(t)
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
