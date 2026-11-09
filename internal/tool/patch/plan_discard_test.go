package patch

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
)

func countOpenFDs(t *testing.T) int {
	t.Helper()
	entries, err := os.ReadDir("/proc/self/fd")
	if err != nil {
		t.Skipf("fd counting unavailable: %v", err)
	}
	return len(entries)
}

func TestPatchFailedPlanReleasesPins(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("requires /proc/self/fd")
	}
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.txt", "before\n")
	writeFixture(t, cwd, "z.txt", "taken\n")
	before := countOpenFDs(t)
	editor := New(string(Replace))
	_, err := editor.Plan(t.Context(), tool.Call{ID: "call", Args: []byte(`{"changes":[{"path":"a.txt","old":"before","new":"after"},{"path":"z.txt","create":"new\n"}]}`)}, env)
	if err == nil {
		t.Fatal("expected plan to fail")
	}
	if got := countOpenFDs(t); got != before {
		t.Fatalf("failed plan leaked %d fds", got-before)
	}
}

func TestPatchDiscardedPlanReleasesPins(t *testing.T) {
	if runtime.GOOS != "linux" {
		t.Skip("requires /proc/self/fd")
	}
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.txt", "before\n")
	before := countOpenFDs(t)
	plan := planFor(t, env, `{"changes":[{"path":"a.txt","old":"before","new":"after"}]}`)
	if plan.Discard == nil {
		t.Fatal("plan has no Discard")
	}
	if got := countOpenFDs(t); got <= before {
		t.Fatalf("plan holds no fds: before %d after %d", before, got)
	}
	plan.Discard()
	if got := countOpenFDs(t); got != before {
		t.Fatalf("discarded plan leaked %d fds", got-before)
	}
	plan.Discard()
	if got := countOpenFDs(t); got != before {
		t.Fatalf("second Discard leaked %d fds", got-before)
	}
}

func TestPatchDiscardAfterApplyIsSafe(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.txt", "before\n")
	plan := planFor(t, env, `{"changes":[{"path":"a.txt","old":"before","new":"after"}]}`)
	if plan.Discard == nil {
		t.Fatal("plan has no Discard")
	}
	if _, err := plan.Apply(t.Context()); err != nil {
		t.Fatalf("apply failed: %v", err)
	}
	plan.Discard()
	plan.Discard()
	requireFile(t, filepath.Join(cwd, "a.txt"), "after\n")
}
