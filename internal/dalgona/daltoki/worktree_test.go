package daltoki

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/github"
)

func seedRemote(t *testing.T, remoteBase string) {
	t.Helper()
	ctx := context.Background()
	bare := filepath.Join(remoteBase, "octo", "repo.git")
	work := t.TempDir()
	if err := os.MkdirAll(bare, 0o700); err != nil {
		t.Fatalf("mkdir remote: %v", err)
	}
	if err := os.WriteFile(filepath.Join(work, "README.md"), []byte("seed\n"), 0o600); err != nil {
		t.Fatalf("write seed: %v", err)
	}
	steps := [][]string{
		{bare, "init", "--quiet", "--bare"},
		{work, "init", "--quiet", "--initial-branch", "main"},
		{work, "add", "README.md"},
		{work, "commit", "--quiet", "--message", "seed"},
		{work, "push", "--quiet", bare, "main"},
	}
	for _, step := range steps {
		if _, err := runGit(ctx, step[0], nil, step[1:]...); err != nil {
			t.Fatalf("seed remote: %v", err)
		}
	}
}

func digestOf(content string) string {
	sum := sha256.Sum256([]byte(content))
	return hex.EncodeToString(sum[:])
}

func TestWorktreesPrepareCommitAndPush(t *testing.T) {
	ctx := context.Background()
	remoteBase := t.TempDir() + string(filepath.Separator)
	seedRemote(t, remoteBase)
	repo := github.Repository{Owner: "octo", Name: "repo"}
	trees := NewWorktrees(t.TempDir(), remoteBase, nil)
	request := WorktreeRequest{JobID: "j-1", Repository: repo, Base: "main", Branch: "daltoki/j-1"}
	tree := requireSeededTree(t, ctx, trees, request)
	commit := requireFixCommitted(t, ctx, trees, tree)
	requirePushSent(t, ctx, trees, repo, commit)
	requireRemoteBranch(t, ctx, remoteBase, commit)
	requireRerunClean(t, ctx, trees, request, repo)
}

func requireSeededTree(t *testing.T, ctx context.Context, trees *Worktrees, request WorktreeRequest) Worktree {
	t.Helper()
	tree, err := trees.Prepare(ctx, request)
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	if seed, seedErr := os.ReadFile(filepath.Join(tree.Dir, "README.md")); seedErr != nil || string(seed) != "seed\n" {
		t.Fatalf("checkout README = %q, %v", seed, seedErr)
	}
	writeFile(t, filepath.Join(tree.Dir, "README.md"), "fixed\n")
	writeFile(t, filepath.Join(tree.Dir, "pkg", "fix.go"), "package pkg\n")
	changes, err := trees.Changes(ctx, tree.Dir)
	if err != nil {
		t.Fatalf("Changes: %v", err)
	}
	want := map[string]string{"README.md": digestOf("fixed\n"), "pkg/fix.go": digestOf("package pkg\n")}
	if len(changes) != len(want) || changes["README.md"] != want["README.md"] || changes["pkg/fix.go"] != want["pkg/fix.go"] {
		t.Fatalf("changes = %v, want %v", changes, want)
	}
	return tree
}

func requireFixCommitted(t *testing.T, ctx context.Context, trees *Worktrees, tree Worktree) string {
	t.Helper()
	commit, err := trees.Commit(ctx, tree.Dir, "Fix #7: crash")
	if err != nil {
		t.Fatalf("Commit: %v", err)
	}
	return commit
}

func requirePushSent(t *testing.T, ctx context.Context, trees *Worktrees, repo github.Repository, commit string) {
	t.Helper()
	if pushErr := trees.Push(ctx, PushIntent{Repository: repo, Branch: "daltoki/j-1", Commit: commit}); pushErr != nil {
		t.Fatalf("Push: %v", pushErr)
	}
}

func requireRemoteBranch(t *testing.T, ctx context.Context, remoteBase, commit string) {
	t.Helper()
	pushed, err := runGit(ctx, filepath.Join(remoteBase, "octo", "repo.git"), nil, "rev-parse", "refs/heads/daltoki/j-1")
	if err != nil || strings.TrimSpace(pushed) != commit {
		t.Fatalf("remote branch = %q, %v, want %s", pushed, err, commit)
	}
}

func requireRerunClean(t *testing.T, ctx context.Context, trees *Worktrees, request WorktreeRequest, repo github.Repository) {
	t.Helper()
	rerun, err := trees.Prepare(ctx, request)
	if err != nil {
		t.Fatalf("Prepare rerun: %v", err)
	}
	if again, err := trees.Changes(ctx, rerun.Dir); err != nil || len(again) != 0 {
		t.Fatalf("rerun changes = %v, %v", again, err)
	}
	if _, err := os.Stat(filepath.Join(rerun.Dir, "pkg", "fix.go")); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("rerun kept prior output: %v", err)
	}
	if err := trees.Remove(ctx, repo, "j-1"); err != nil {
		t.Fatalf("Remove: %v", err)
	}
	if _, err := os.Stat(rerun.Dir); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("worktree still present: %v", err)
	}
}

func TestWorktreesRefuseUnsafeRefs(t *testing.T) {
	ctx := context.Background()
	trees := NewWorktrees(t.TempDir(), t.TempDir()+string(filepath.Separator), nil)
	repo := github.Repository{Owner: "octo", Name: "repo"}
	if _, err := trees.Prepare(ctx, WorktreeRequest{JobID: "j-1", Repository: repo, Base: "--upload-pack=x"}); err == nil {
		t.Fatal("Prepare accepted an option-shaped base branch")
	}
	if _, err := trees.Prepare(ctx, WorktreeRequest{JobID: "j-1", Repository: repo, Base: "main:evil"}); err == nil {
		t.Fatal("Prepare accepted a refspec-shaped base branch")
	}
	if err := trees.Push(ctx, PushIntent{Repository: repo, Branch: "daltoki/j-1", Commit: "HEAD:refs/heads/main"}); err == nil {
		t.Fatal("Push accepted a refspec-shaped commit")
	}
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatalf("write: %v", err)
	}
}

func TestChangesSeesCommittedWork(t *testing.T) {
	ctx := context.Background()
	remoteBase := t.TempDir() + string(filepath.Separator)
	seedRemote(t, remoteBase)
	repo := github.Repository{Owner: "octo", Name: "repo"}
	trees := NewWorktrees(t.TempDir(), remoteBase, nil)
	tree, err := trees.Prepare(ctx, WorktreeRequest{JobID: "j-1", Repository: repo, Base: "main", Branch: "daltoki/j-1"})
	if err != nil {
		t.Fatalf("Prepare: %v", err)
	}
	writeFile(t, filepath.Join(tree.Dir, "fix.go"), "package fix\n")
	if _, commitErr := trees.Commit(ctx, tree.Dir, "Fix #7: crash"); commitErr != nil {
		t.Fatalf("Commit: %v", commitErr)
	}
	writeFile(t, filepath.Join(tree.Dir, "notes.txt"), "follow-up\n")
	changes, err := trees.Changes(ctx, tree.Dir)
	if err != nil {
		t.Fatalf("Changes: %v", err)
	}
	if changes["fix.go"] != digestOf("package fix\n") {
		t.Fatalf("committed change missing: %v", changes)
	}
	if changes["notes.txt"] != digestOf("follow-up\n") {
		t.Fatalf("uncommitted change missing: %v", changes)
	}
}
