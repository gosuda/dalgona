package patch

import (
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

type installThenFailRunner struct {
	runner.Runner
	target string
}

func (r installThenFailRunner) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	if err := r.Runner.WriteFile(ctx, path, data, perm); err != nil {
		return err
	}
	if path == r.target {
		return errors.New("injected sync failure")
	}
	return nil
}

type blindRunner struct {
	runner.Runner
	failWrite string
	blind     string
	armed     bool
}

func (r *blindRunner) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	if path == r.failWrite {
		r.armed = true
		return errors.New("injected second-file failure")
	}
	return r.Runner.WriteFile(ctx, path, data, perm)
}

func (r *blindRunner) ReadFile(ctx context.Context, path string) ([]byte, error) {
	if r.armed && path == r.blind {
		return nil, errors.New("injected read failure")
	}
	return r.Runner.ReadFile(ctx, path)
}

type linkedFailRunner struct {
	runner.Runner
	failing map[string]bool
}

func (r linkedFailRunner) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	if r.failing[path] {
		return errors.New("injected write failure")
	}
	return r.Runner.WriteFile(ctx, path, data, perm)
}

func (r linkedFailRunner) LinkFile(_ context.Context, oldpath, newpath string) error {
	return os.Link(oldpath, newpath)
}

type failSecondStageRunner struct {
	runner.Runner
	staged int
}

func (r *failSecondStageRunner) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	if strings.Contains(path, ".dalgona-tmp-") {
		r.staged++
		if r.staged == 2 {
			return errors.New("injected staging failure")
		}
	}
	return r.Runner.WriteFile(ctx, path, data, perm)
}

type flipContext struct {
	context.Context
	left int
}

func (c *flipContext) Err() error {
	if c.left > 0 {
		c.left--
		return nil
	}
	return context.Canceled
}

var (
	installThenFail = wrapWith(func(run runner.Runner, cwd string) runner.Runner {
		return installThenFailRunner{Runner: run, target: filepath.Join(cwd, "a.txt")}
	})
	blindAfterFailure = wrapWith(func(run runner.Runner, cwd string) runner.Runner {
		return &blindRunner{Runner: run, failWrite: filepath.Join(cwd, "b.txt"), blind: filepath.Join(cwd, "a.txt")}
	})
	silentRestore = wrapWith(func(run runner.Runner, cwd string) runner.Runner {
		return &failRestoreRunner{Runner: run, cwd: cwd, silent: true}
	})
	failSecondStage = wrapWith(func(run runner.Runner, _ string) runner.Runner {
		return &failSecondStageRunner{Runner: run}
	})
	renameWithFailingRestore = wrapWith(func(run runner.Runner, cwd string) runner.Runner {
		failing := map[string]bool{filepath.Join(cwd, "a.txt"): true, filepath.Join(cwd, "b.txt"): true}
		return linkedFailRunner{Runner: run, failing: failing}
	})
)

const (
	absent          = "\x00absent"
	threeFileEdit   = twoFileEdit + "[c.txt@{refC}]\nPUT 1:\n+c-after\n"
	installOneEdit  = "[a.txt@{refA}]\nPUT 1:\n+a-after\n"
	renameFirstEdit = "[a.txt@{refA}]\nMV z.txt\n[b.txt@{refB}]\nPUT 1:\n+b-after\n"
)

type image struct{ before, after string }

func threeFiles(t *testing.T, f *outcomeFixture) {
	t.Helper()
	twoFiles(t, f)
	writeFixture(t, f.cwd, "c.txt", "c-before\n")
	f.refs["refC"] = seedReference(f.env, "c.txt", "c-before\n", true)
}

func shaOf(content string) string {
	if content == absent {
		return ""
	}
	return snapshot.Digest([]byte(content))
}

func diskSHA(t *testing.T, cwd, name string) string {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(cwd, name))
	if errors.Is(err, fs.ErrNotExist) {
		return ""
	}
	if err != nil {
		t.Fatal(err)
	}
	return snapshot.Digest(data)
}

func requireStatesMatchDisk(t *testing.T, cwd string, paths []tool.PathState, images map[string]image) {
	t.Helper()
	for _, item := range paths {
		want := shaOf(images[item.Path].before)
		if item.State == "after" {
			want = shaOf(images[item.Path].after)
		}
		if got := diskSHA(t, cwd, item.Path); got != want {
			t.Errorf("%s is reported %s but its bytes hash to %q, not %q", item.Path, item.State, got, want)
		}
	}
}

func scratchFiles(t *testing.T, cwd, marker string) []string {
	t.Helper()
	var found []string
	err := filepath.WalkDir(cwd, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if strings.Contains(entry.Name(), marker) {
			found = append(found, path)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	return found
}

func requireNoResidue(t *testing.T, cwd string) {
	t.Helper()
	if left := scratchFiles(t, cwd, ".dalgona-"); len(left) != 0 {
		t.Fatalf("patch residue remains: %v", left)
	}
}

func retainedBackup(t *testing.T, text, name string) string {
	t.Helper()
	_, rest, ok := strings.Cut(text, "Retained backup for "+name+": ")
	if !ok {
		t.Fatalf("error does not name the backup of %s: %s", name, text)
	}
	backup, _, _ := strings.Cut(rest, "\n")
	return strings.TrimSpace(backup)
}

func requireBackupHolds(t *testing.T, text, name, content string) {
	t.Helper()
	backup := retainedBackup(t, text, name)
	data, err := os.ReadFile(backup)
	if err != nil || string(data) != content {
		t.Fatalf("backup of %s = %q, %v; want %q", name, data, err, content)
	}
}

func outcomeOfError(t *testing.T, err error) tool.Outcome {
	t.Helper()
	var failure *tool.OutcomeError
	if !errors.As(err, &failure) {
		t.Fatalf("error %v carries no outcome", err)
	}
	return failure.Outcome
}

func TestPartialWriteListsEveryPath(t *testing.T) {
	images := map[string]image{
		"a.txt": {"a-before\n", "a-after\n"},
		"b.txt": {"b-before\n", "b-after\n"},
		"c.txt": {"c-before\n", "c-after\n"},
	}
	t.Run("IO-03/restore failed", func(t *testing.T) {
		f := newOutcomeFixture(t)
		threeFiles(t, f)
		failRestore(t, f)
		err := f.run(t, HashlineLight, threeFileEdit)
		want := withPaths(writeFailure(tool.CommitPartial, tool.ReasonRollbackFailed), "a.txt", "after", "b.txt", "before", "c.txt", "before")
		requireWrappedOutcome(t, err, HashlineLight, want)
		requireStatesMatchDisk(t, f.cwd, outcomeOfError(t, err).Paths, images)
		requireBackupHolds(t, err.Error(), "a.txt", "a-before\n")
	})
	t.Run("IO-03/restore reported success but wrote nothing", func(t *testing.T) {
		f := newOutcomeFixture(t)
		threeFiles(t, f)
		silentRestore(t, f)
		err := f.run(t, HashlineLight, threeFileEdit)
		want := withPaths(writeFailure(tool.CommitPartial, tool.ReasonSyncFailed), "a.txt", "after", "b.txt", "before", "c.txt", "before")
		requireWrappedOutcome(t, err, HashlineLight, want)
		requireStatesMatchDisk(t, f.cwd, outcomeOfError(t, err).Paths, images)
		requireNoResidue(t, f.cwd)
	})
	t.Run("IO-03/rename source and destination", func(t *testing.T) {
		f := newOutcomeFixture(t)
		twoFiles(t, f)
		renameWithFailingRestore(t, f)
		err := f.run(t, HashlineLight, renameFirstEdit)
		want := withPaths(writeFailure(tool.CommitPartial, tool.ReasonRollbackFailed), "a.txt", "after", "z.txt", "before", "b.txt", "before")
		requireWrappedOutcome(t, err, HashlineLight, want)
		requireStatesMatchDisk(t, f.cwd, outcomeOfError(t, err).Paths, map[string]image{
			"a.txt": {"a-before\n", absent},
			"z.txt": {absent, "a-before\n"},
			"b.txt": images["b.txt"],
		})
		requireBackupHolds(t, err.Error(), "a.txt", "a-before\n")
	})
}

func TestRestoredWriteIsRestoredNotNone(t *testing.T) {
	t.Run("IO-02/restored", func(t *testing.T) {
		f := newOutcomeFixture(t)
		twoFiles(t, f)
		failSecondFile(t, f)
		err := f.run(t, HashlineLight, twoFileEdit)
		want := withPaths(writeFailure(tool.CommitRestored, tool.ReasonWriteFailed), "a.txt", "before", "b.txt", "before")
		requireWrappedOutcome(t, err, HashlineLight, want)
		if !strings.Contains(err.Error(), "All completed writes were restored") {
			t.Fatalf("error does not say the writes were restored: %v", err)
		}
		for name, body := range map[string]string{"a.txt": "a-before\n", "b.txt": "b-before\n"} {
			if actual := readFixture(t, f.cwd, name); actual != body {
				t.Fatalf("%s = %q, want %q", name, actual, body)
			}
		}
		requireNoResidue(t, f.cwd)
	})
}

func TestSyncFailureAfterInstallIsComplete(t *testing.T) {
	t.Run("IO-05/one unpinned file", func(t *testing.T) {
		f := newOutcomeFixture(t)
		twoFiles(t, f)
		installThenFail(t, f)
		err := f.run(t, HashlineLight, installOneEdit)
		want := withPaths(writeFailure(tool.CommitComplete, tool.ReasonSyncFailed), "a.txt", "after")
		requireWrappedOutcome(t, err, HashlineLight, want)
		if actual := readFixture(t, f.cwd, "a.txt"); actual != "a-after\n" {
			t.Fatalf("a.txt = %q, want the installed bytes", actual)
		}
		requireNoResidue(t, f.cwd)
	})
}

func TestIndeterminateWhenShaUnreadable(t *testing.T) {
	t.Run("IO-04/backups retained", func(t *testing.T) {
		f := newOutcomeFixture(t)
		twoFiles(t, f)
		blindAfterFailure(t, f)
		err := f.run(t, HashlineLight, twoFileEdit)
		requireWrappedOutcome(t, err, HashlineLight, withPaths(commitUnknown, "a.txt", "unknown", "b.txt", "before"))
		requireBackupHolds(t, err.Error(), "a.txt", "a-before\n")
		requireBackupHolds(t, err.Error(), "b.txt", "b-before\n")
		temps, globErr := filepath.Glob(filepath.Join(f.cwd, "*.dalgona-tmp-*"))
		if globErr != nil || len(temps) != 0 {
			t.Fatalf("temporary files remain: %v, %v", temps, globErr)
		}
	})
	t.Run("IO-04/unreadable file that was to be deleted", func(t *testing.T) {
		f := newOutcomeFixture(t)
		twoFiles(t, f)
		blindAfterFailure(t, f)
		err := f.run(t, HashlineLight, "[a.txt@{refA}]\nREM\n[b.txt@{refB}]\nPUT 1:\n+b-after\n")
		requireWrappedOutcome(t, err, HashlineLight, withPaths(commitUnknown, "a.txt", "unknown", "b.txt", "before"))
		requireBackupHolds(t, err.Error(), "a.txt", "a-before\n")
	})
}

type partialEpochCase struct {
	name        string
	input       string
	steps       []fixtureStep
	invalidated bool
}

func (test partialEpochCase) run(t *testing.T) {
	f := newOutcomeFixture(t)
	plain := f.env.Runner
	for _, step := range test.steps {
		step(t, f)
	}
	if err := f.run(t, HashlineLight, test.input); err == nil {
		t.Fatal("the injected failure did not fail the call")
	}
	f.env.Runner = plain
	writeFixture(t, f.cwd, "a.txt", "a-before\n")
	err := f.run(t, HashlineLight, "[a.txt@{refA}]\nPUT 1:\n+a-second\n")
	if !test.invalidated {
		if err != nil {
			t.Fatalf("a reference over verified bytes was refused: %v", err)
		}
		return
	}
	want := withRecovery(refused(tool.StatusConflict, tool.ReasonPathChanged, tool.ErrClassStale), readUnknown("a.txt"))
	requireWrappedOutcome(t, err, HashlineLight, want)
	if actual := readFixture(t, f.cwd, "a.txt"); actual != "a-before\n" {
		t.Fatalf("a refused call changed a.txt to %q", actual)
	}
}

func TestPartialOutcomeInvalidatesEpoch(t *testing.T) {
	tests := []partialEpochCase{
		{"C13/partial", twoFileEdit, []fixtureStep{twoFiles, failRestore}, true},
		{"C13/unknown", twoFileEdit, []fixtureStep{twoFiles, blindAfterFailure}, true},
		{"C13/complete", installOneEdit, []fixtureStep{twoFiles, installThenFail}, true},
		{"C13/restored", twoFileEdit, []fixtureStep{twoFiles, failSecondFile}, false},
	}
	for _, test := range tests {
		t.Run(test.name, test.run)
	}
}

func TestStagingFailureIsWriteErrorNone(t *testing.T) {
	t.Run("IO-01/second file", func(t *testing.T) {
		f := newOutcomeFixture(t)
		twoFiles(t, f)
		failSecondStage(t, f)
		err := f.run(t, HashlineLight, twoFileEdit)
		requireWrappedOutcome(t, err, HashlineLight, writeFailure(tool.CommitNone, tool.ReasonWriteFailed))
		if !strings.Contains(err.Error(), "injected staging failure") {
			t.Fatalf("error does not carry the cause: %v", err)
		}
		for name, body := range map[string]string{"a.txt": "a-before\n", "b.txt": "b-before\n"} {
			if actual := readFixture(t, f.cwd, name); actual != body {
				t.Fatalf("%s = %q, want %q", name, actual, body)
			}
		}
		requireNoResidue(t, f.cwd)
	})
}

func TestCancelBeforeFirstMutationIsInterrupted(t *testing.T) {
	t.Run("IO-07/before the first effect", func(t *testing.T) {
		f := newOutcomeFixture(t)
		plan, err := f.plan(t, HashlineLight, "[f.txt@{ref}]\nPUT 1:\n+uno\n")
		if err != nil {
			t.Fatal(err)
		}
		_, err = plan.Apply(&flipContext{Context: t.Context(), left: 1})
		want := withPaths(refused(tool.StatusInterrupted, tool.ReasonInterrupted, tool.ErrClassNone), "f.txt", "before")
		requireWrappedOutcome(t, err, HashlineLight, want)
		if actual := readFixture(t, f.cwd, "f.txt"); actual != outcomeSource {
			t.Fatalf("f.txt = %q, want the original bytes", actual)
		}
		requireNoResidue(t, f.cwd)
	})
}

func TestLegacyCommitFailureIsUnchanged(t *testing.T) {
	t.Run("COMPAT-01/replace", func(t *testing.T) {
		env, cwd := editTestEnv(t)
		writeFixture(t, cwd, "a.txt", "a-before\n")
		writeFixture(t, cwd, "b.txt", "b-before\n")
		ref := env.Snapshots.Put("a.txt", []byte("a-before\n"), nil)
		env.Runner = &failRestoreRunner{Runner: env.Runner, cwd: cwd}
		input := `{"changes":[{"path":"a.txt","old":"a-before","new":"a-after"},{"path":"b.txt","old":"b-before","new":"b-after"}]}`
		_, err := runPatch(t, env, Replace, input)
		requireNoOutcome(t, err)
		backups, globErr := filepath.Glob(filepath.Join(cwd, "*.dalgona-backup-*"))
		if globErr != nil || len(backups) != 2 {
			t.Fatalf("backups = %v, %v; want one per file", backups, globErr)
		}
		backupA, backupB := backups[0], backups[1]
		want := "patch: commit failed at b.txt: patch: cannot write " + filepath.Join(cwd, "b.txt") +
			": injected second-file failure. Rollback was incomplete. Affected paths: a.txt, b.txt." +
			"\n" + filepath.Join(cwd, "a.txt") + ": injected restore failure (backup " + backupA + ")" +
			"\nRetained backup for a.txt: " + backupA + "\nRetained backup for b.txt: " + backupB
		if err.Error() != want {
			t.Fatalf("legacy error text changed:\n%s\nwant:\n%s", err, want)
		}
		writeFixture(t, cwd, "a.txt", "a-before\n")
		if !env.Snapshots.Unchanged(ref, "a.txt", []byte("a-before\n")) {
			t.Fatal("a legacy commit failure invalidated an old reference")
		}
	})
}
