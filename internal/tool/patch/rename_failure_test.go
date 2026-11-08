package patch

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
)

type failRemoveRunner struct {
	runner.Runner
	target string
	failed bool
	linked bool
}

func (r *failRemoveRunner) LinkFile(ctx context.Context, oldpath, newpath string) error {
	if err := os.Link(oldpath, newpath); err != nil {
		return err
	}
	r.linked = true
	return nil
}

func (r *failRemoveRunner) Remove(ctx context.Context, path string) error {
	if path == r.target && !r.failed {
		r.failed = true
		return errors.New("injected source removal failure")
	}
	return r.Runner.Remove(ctx, path)
}

func TestRenameSourceFailureRemovesDestination(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.txt", "before\n")
	failing := &failRemoveRunner{Runner: env.Runner, target: filepath.Join(cwd, "a.txt")}
	env.Runner = failing
	_, err := runPatch(t, env, Replace, `{"changes":[{"path":"a.txt","rename":"b.txt"}]}`)
	if err == nil {
		t.Fatal("injected rename failure succeeded")
	}
	if actual := readFixture(t, cwd, "a.txt"); actual != "before\n" {
		t.Fatalf("rename source changed %q", actual)
	}
	if !failing.linked {
		t.Fatal("test did not publish destination before injected failure")
	}
	if _, statErr := os.Stat(filepath.Join(cwd, "b.txt")); !errors.Is(statErr, os.ErrNotExist) {
		t.Fatalf("destination survived rollback: %v", statErr)
	}
	if !strings.Contains(err.Error(), "a.txt") || !strings.Contains(err.Error(), "b.txt") {
		t.Fatalf("error does not name both paths: %v", err)
	}
}
