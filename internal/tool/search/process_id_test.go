package search

import (
	"encoding/json/jsontext"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func TestImplicitSearchIDsDoNotReplayOutput(t *testing.T) {
	root := t.TempDir()
	writeSearchFixture(t, root, "f.txt", "alpha\n")
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = local.Close() })
	process := &fakeProcessRunner{Runner: local}
	env := tool.Env{Runner: process, Cwd: root, Snapshots: snapshot.New()}
	args := jsontext.Value(`{"mode":"grep","pattern":"alpha"}`)
	if _, err := New().Run(t.Context(), tool.Call{Args: args}, env); err != nil {
		t.Fatal(err)
	}
	if _, err := New().Run(t.Context(), tool.Call{Args: args}, env); err != nil {
		t.Fatal(err)
	}
	if len(process.ids) != 2 || process.ids[0] == process.ids[1] {
		t.Fatalf("implicit process IDs were reused: %v", process.ids)
	}
}
