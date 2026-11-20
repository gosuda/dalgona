package llmstub

import (
	"errors"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
)

func startLiveWorkspace(t *testing.T, root, product string, wantRefusal bool) {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("the live recipes use Bash on Unix")
	}
	const start = `source live-workspace.sh
prepare_live_workspace "$1" "$2"
finish_live_start 0 "$1" "$2"`
	out, err := exec.CommandContext(t.Context(), "bash", "-eu", "-c", start, "live-up", root, product).CombinedOutput()
	if wantRefusal {
		if err == nil || !strings.Contains(string(out), "cannot reset") {
			t.Fatalf("start in %s: %v, %s; want workspace refusal", root, err, out)
		}
	} else if err != nil {
		t.Fatalf("start in %s: %v, %s", root, err, out)
	}
}

func TestLiveWorkspacePreservesUnmarkedDirectory(t *testing.T) {
	t.Parallel()
	root := t.TempDir()
	note := filepath.Join(root, "notes")
	if err := os.WriteFile(note, []byte("keep"), 0o600); err != nil {
		t.Fatal(err)
	}
	startLiveWorkspace(t, root, "dal", true)
	if got, err := os.ReadFile(note); err != nil || string(got) != "keep" {
		t.Fatalf("read existing notes: %q, %v", got, err)
	}
}

func TestLiveWorkspaceResetsOnlyItsOwnDirectory(t *testing.T) {
	t.Parallel()
	parent := t.TempDir()
	root := filepath.Join(parent, "live", "workspace")
	startLiveWorkspace(t, root, "dal", false)
	note := filepath.Join(root, "first-run")
	if err := os.WriteFile(note, nil, 0o600); err != nil {
		t.Fatal(err)
	}
	link := filepath.Join(parent, "link")
	if err := os.Symlink(root, link); err != nil {
		t.Fatal(err)
	}
	startLiveWorkspace(t, link+"/", "dal", true)
	startLiveWorkspace(t, root, "dalgona", true)
	if _, err := os.Stat(note); err != nil {
		t.Fatalf("first run's files after refused starts: %v", err)
	}
	startLiveWorkspace(t, root, "dal", false)
	if _, err := os.Stat(note); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("first run's files after restart: %v", err)
	}
}

func TestLivePIDRunningRecognizesAnUnreapedExit(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("the live recipes use Bash on Unix")
	}
	child := exec.CommandContext(t.Context(), "true")
	if err := child.Start(); err != nil {
		t.Fatal(err)
	}
	defer func() {
		if err := child.Wait(); err != nil {
			t.Error(err)
		}
	}()
	const check = `source live-workspace.sh
live_pid_running "$PPID" || exit 1
for _ in {1..50}; do
    live_pid_running "$1" || exit 0
    sleep 0.02
done
exit 1`
	if err := exec.CommandContext(t.Context(), "bash", "-eu", "-c", check, "live-pid", strconv.Itoa(child.Process.Pid)).Run(); err != nil {
		t.Fatalf("expected the parent to be running and the exited child to be stopped: %v", err)
	}
}
