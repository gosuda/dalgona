package remote

import (
	"bytes"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"testing"
)

func TestApplyDiffReportsConflicts(t *testing.T) {
	cwd := gitWorkspace(t)
	writeAndRun := func(content string, args ...string) []byte {
		t.Helper()
		if err := os.WriteFile(filepath.Join(cwd, "hello.txt"), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
		cmd := exec.Command("git", append([]string{"-c", "user.name=test", "-c", "user.email=test@example.com"}, args...)...)
		cmd.Dir = cwd
		out, err := cmd.Output()
		if err != nil {
			t.Fatalf("git %v: %v", args, err)
		}
		return out
	}
	diff := writeAndRun("two from box\n", "diff", "--no-ext-diff", "--no-color", "--binary")
	writeAndRun("three from local\n", "commit", "-qam", "local")

	err := applyDiff(t.Context(), cwd, diff)
	conflict, ok := errors.AsType[*ConflictError](err)
	if !ok || !slices.Equal(conflict.Files, []string{"hello.txt"}) {
		t.Fatalf("applyDiff error = %v, want conflict in hello.txt", err)
	}
	var stderr bytes.Buffer
	if code := report(&stderr, err); code != 1 {
		t.Fatalf("exit code %d, want 1", code)
	}
	if want := "remote: pull left conflict markers in:\n  hello.txt\n"; stderr.String() != want {
		t.Fatalf("stderr %q, want %q", stderr.String(), want)
	}
}

func TestApplyDiffInRepoSubdirectory(t *testing.T) {
	box := gitWorkspace(t)
	must(t, os.WriteFile(filepath.Join(box, "hello.txt"), []byte("two from box\n"), 0o644))
	diffCmd := exec.Command("git", "diff", "--no-ext-diff", "--no-color", "--binary")
	diffCmd.Dir = box
	diff, err := diffCmd.Output()
	must(t, err)

	repo := gitWorkspace(t)
	sub := filepath.Join(repo, "sub")
	must(t, os.Mkdir(sub, 0o755))
	must(t, os.Rename(filepath.Join(repo, "hello.txt"), filepath.Join(sub, "hello.txt")))
	must(t, exec.Command("git", "-C", repo, "add", "-A").Run())
	commit := exec.Command("git", "-c", "user.name=test", "-c", "user.email=test@example.com", "commit", "-qam", "move")
	commit.Dir = repo
	if out, err := commit.CombinedOutput(); err != nil {
		t.Fatalf("commit: %v: %s", err, out)
	}

	must(t, applyDiff(t.Context(), sub, diff))
	requireFiles(t, sub, map[string]string{"hello.txt": "two from box\n"})
}
