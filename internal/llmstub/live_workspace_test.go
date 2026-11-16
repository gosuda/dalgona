package llmstub

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

const liveWorkspace = "testdata/workspace"

func TestLiveWorkspaceFailOutputIsExecutable(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("file modes carry no execute bit on Windows")
	}
	script, err := os.Stat(filepath.Join(liveWorkspace, "fail-output.sh"))
	if err != nil {
		t.Fatal(err)
	}
	if script.Mode().Perm()&0o100 == 0 {
		t.Fatalf("fail-output.sh mode = %v, want the owner execute bit", script.Mode().Perm())
	}
	control, err := os.Stat(filepath.Join(liveWorkspace, "a.txt"))
	if err != nil {
		t.Fatal(err)
	}
	if control.Mode().Perm()&0o111 != 0 {
		t.Fatalf("a.txt mode = %v, want no execute bit", control.Mode().Perm())
	}
}

func TestLiveWorkspaceFailOutputFollowsTheT12Contract(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("the fixture is a POSIX shell script")
	}
	var stdout, stderr bytes.Buffer
	cmd := exec.CommandContext(t.Context(), "./fail-output.sh")
	cmd.Dir = liveWorkspace
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := cmd.Run()
	exit, ok := errors.AsType[*exec.ExitError](err)
	if !ok || exit.ExitCode() != 3 {
		t.Fatalf("run error = %v, want exit status 3", err)
	}
	var want strings.Builder
	for i := 1; i <= 30; i++ {
		fmt.Fprintln(&want, i)
	}
	if stdout.String() != want.String() {
		t.Fatalf("stdout = %q, want the lines 1 to 30", stdout.String())
	}
	if stderr.String() != "boom\n" {
		t.Fatalf("stderr = %q, want boom", stderr.String())
	}
}
