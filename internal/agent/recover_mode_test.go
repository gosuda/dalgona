package agent

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/event"
)

func mustChmod(t *testing.T, path string, mode os.FileMode) {
	t.Helper()
	if err := os.Chmod(path, mode); err != nil {
		t.Fatal(err)
	}
}

func recordedMode(t *testing.T, path string) os.FileMode {
	t.Helper()
	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	return info.Mode().Perm()
}

func checkModes(t *testing.T, cwd string, wants map[string]os.FileMode) {
	t.Helper()
	for path, want := range wants {
		info, err := os.Stat(filepath.Join(cwd, path))
		if err != nil {
			t.Fatal(err)
		}
		if info.Mode().Perm() != want {
			t.Fatalf("%s mode = %o, want %o", path, info.Mode().Perm(), want)
		}
	}
}

func TestRecoverEditRestoresFileMode(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	scriptPre := "#!/bin/sh\nexit 0\n"
	scriptPost := "#!/bin/sh\nexit 1\n"
	secretPre := "before\n"
	secretPost := "after\n"
	h.write("run.sh", scriptPost)
	h.write("secret.txt", secretPost)
	h.write("other.txt", "foreign\n")
	mustChmod(t, filepath.Join(h.cwd, "run.sh"), 0o755)
	mustChmod(t, filepath.Join(h.cwd, "secret.txt"), 0o600)
	runMode := recordedMode(t, filepath.Join(h.cwd, "run.sh"))
	secretMode := recordedMode(t, filepath.Join(h.cwd, "secret.txt"))
	script := h.fileChange("run.sh", scriptPre, scriptPost)
	secret := h.fileChange("secret.txt", secretPre, secretPost)
	other := h.fileChange("other.txt", "other-old\n", "other-new\n")
	args := `{"changes":[{"path":"run.sh","old":"old","new":"new"}]}`
	h.seed(prefix(openTurn("edit"), []event.Event{assistantCall("c1", "patch", args)}, approved("c1", "edit"),
		[]event.Event{startedCall("c1", "patch", "edit", args, script, secret, other)})...)
	got := completedOutputs(h.recoverSession())["c1"]
	if got.OK || got.Recovered != "interrupted" {
		t.Fatalf("completion = %+v", got)
	}
	if h.read("run.sh") != scriptPre {
		t.Fatalf("run.sh = %q", h.read("run.sh"))
	}
	if h.read("secret.txt") != secretPre {
		t.Fatalf("secret.txt = %q", h.read("secret.txt"))
	}
	checkModes(t, h.cwd, map[string]os.FileMode{"run.sh": runMode, "secret.txt": secretMode})
	if h.read("other.txt") != "foreign\n" {
		t.Fatalf("other.txt = %q", h.read("other.txt"))
	}
}
