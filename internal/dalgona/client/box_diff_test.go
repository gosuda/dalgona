package client

import (
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"testing"
)

func runGit(t *testing.T, dir string, args ...string) []byte {
	t.Helper()
	cmd := exec.Command("git", append([]string{"-c", "user.name=test", "-c", "user.email=test@example.com", "-c", "core.autocrlf=false"}, args...)...)
	cmd.Dir = dir
	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git %v: %v", args, err)
	}
	return out
}

func gitWorkspace(t *testing.T) string {
	t.Helper()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "hello.txt"), []byte("one\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	runGit(t, dir, "init", "-q")
	runGit(t, dir, "config", "core.autocrlf", "false")
	runGit(t, dir, "config", "core.eol", "lf")
	runGit(t, dir, "add", "-A")
	runGit(t, dir, "commit", "-q", "-m", "init")
	return dir
}

func writeHello(t *testing.T, dir, content string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, "hello.txt"), []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestApplyDiffReportsConflicts(t *testing.T) {
	cwd := gitWorkspace(t)
	writeHello(t, cwd, "two from box\n")
	diff := runGit(t, cwd, "diff", "--no-ext-diff", "--no-color", "--binary")
	writeHello(t, cwd, "three from local\n")
	runGit(t, cwd, "commit", "-qam", "local")

	err := ApplyDiff(t.Context(), cwd, diff)
	conflict, ok := errors.AsType[*ConflictError](err)
	if !ok || !slices.Equal(conflict.Files, []string{"hello.txt"}) {
		t.Fatalf("ApplyDiff error = %v, want conflict in hello.txt", err)
	}
	if want := "remote: pull left conflict markers in:\n  hello.txt\n"; conflict.Report() != want {
		t.Fatalf("report %q, want %q", conflict.Report(), want)
	}
}

func TestConflictReportListsEachFileOnItsOwnLine(t *testing.T) {
	conflict := &ConflictError{Files: []string{"a.txt", "dir/b.txt"}}
	if want := "remote: pull left conflict markers in:\n  a.txt\n  dir/b.txt\n"; conflict.Report() != want {
		t.Fatalf("report %q, want %q", conflict.Report(), want)
	}
}

func TestApplyDiffInRepoSubdirectory(t *testing.T) {
	box := gitWorkspace(t)
	writeHello(t, box, "two from box\n")
	diff := runGit(t, box, "diff", "--no-ext-diff", "--no-color", "--binary")
	repo := gitWorkspace(t)
	sub := filepath.Join(repo, "sub")
	if err := os.Mkdir(sub, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(filepath.Join(repo, "hello.txt"), filepath.Join(sub, "hello.txt")); err != nil {
		t.Fatal(err)
	}
	runGit(t, repo, "add", "-A")
	runGit(t, repo, "commit", "-qam", "move")

	if err := ApplyDiff(t.Context(), sub, diff); err != nil {
		t.Fatal(err)
	}
	if got, err := os.ReadFile(filepath.Join(sub, "hello.txt")); err != nil || string(got) != "two from box\n" {
		t.Fatalf("sub/hello.txt = %q, %v", got, err)
	}
}
