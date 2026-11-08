package patch

import (
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
)

type failStageRunner struct{ runner.Runner }

func (r failStageRunner) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	if strings.Contains(path, ".dalgona-tmp-") {
		return errors.New("injected staging failure")
	}
	return r.Runner.WriteFile(ctx, path, data, perm)
}

func TestTemporaryStagingFailureWritesNothing(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", "before\n")
	env.Runner = failStageRunner{Runner: env.Runner}
	_, err := runPatch(t, env, Replace, `{"changes":[{"path":"f.txt","old":"before","new":"after"}]}`)
	if err == nil {
		t.Fatal("staging failure succeeded")
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "before\n" {
		t.Fatalf("staging failure changed target %q", actual)
	}
}

type failRestoreRunner struct {
	runner.Runner
	cwd    string
	aCalls int
	bCalls int
}

func (r *failRestoreRunner) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	switch path {
	case filepath.Join(r.cwd, "a.txt"):
		r.aCalls++
		if r.aCalls == 2 {
			return errors.New("injected restore failure")
		}
	case filepath.Join(r.cwd, "b.txt"):
		r.bCalls++
		if r.bCalls == 1 {
			return errors.New("injected second-file failure")
		}
	}
	return r.Runner.WriteFile(ctx, path, data, perm)
}

func TestRollbackFailureNamesPathsAndRetainedBackup(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.txt", "a-before\n")
	writeFixture(t, cwd, "b.txt", "b-before\n")
	env.Runner = &failRestoreRunner{Runner: env.Runner, cwd: cwd}
	input := `{"changes":[{"path":"a.txt","old":"a-before","new":"a-after"},{"path":"b.txt","old":"b-before","new":"b-after"}]}`
	_, err := runPatch(t, env, Replace, input)
	if err == nil {
		t.Fatal("injected rollback failure succeeded")
	}
	text := err.Error()
	for _, want := range []string{"a.txt", "b.txt", "Rollback was incomplete", "Retained backup for a.txt"} {
		if !strings.Contains(text, want) {
			t.Fatalf("error lacks %q: %s", want, text)
		}
	}
	_, marker, ok := strings.Cut(text, "Retained backup for a.txt: ")
	if !ok {
		t.Fatal("missing backup path")
	}
	backup, _, _ := strings.Cut(marker, "\n")
	if _, statErr := os.Stat(strings.TrimSpace(backup)); statErr != nil {
		t.Fatalf("retained backup missing: %v", statErr)
	}
	if actual := readFixture(t, cwd, "a.txt"); actual != "a-after\n" {
		t.Fatalf("failed restore state %q", actual)
	}
}
