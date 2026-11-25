package patch

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

const replaceTwoFiles = `{"changes":[{"path":"a.txt","old":"a-before","new":"a-after"},{"path":"b.txt","old":"b-before","new":"b-after"}]}`

type cancelPoint int

const (
	cancelAtApply cancelPoint = iota + 1
	cancelMidCommit
)

type failureCase struct {
	name    string
	style   Style
	steps   []fixtureStep
	input   string
	between []fixtureStep
	cancel  cancelPoint
	want    tool.FailureKind
}

func (c failureCase) run(t *testing.T) {
	f := newOutcomeFixture(t)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	for _, step := range c.steps {
		step(t, f)
	}
	if c.cancel == cancelMidCommit {
		f.env.Runner = &cancelAfterFirstRunner{Runner: f.env.Runner, cwd: f.cwd, cancel: cancel}
	}
	input := f.input(c.input)
	args := []byte(input)
	if c.style != Replace {
		args = []byte(`{"input":` + snapshot.JSONString(input) + `}`)
	}
	plan, err := New(string(c.style)).Plan(t.Context(), tool.Call{ID: "call", Args: args}, boundEvidence(f.env, input))
	if err != nil {
		t.Fatalf("plan: %v", err)
	}
	for _, step := range c.between {
		step(t, f)
	}
	if c.cancel == cancelAtApply {
		cancel()
	}
	result, err := plan.Apply(ctx)
	if err == nil {
		t.Fatal("the injected failure did not fail the call")
	}
	if result.Failure != c.want {
		t.Fatalf("failure = %q, want %q (error: %v)", result.Failure, c.want, err)
	}
}

func readOnlyDir(name string) fixtureStep {
	return func(t *testing.T, f *outcomeFixture) {
		t.Helper()
		if runtime.GOOS == "windows" || os.Getuid() == 0 {
			t.Skip("directory permission bits do not refuse this user")
		}
		dir := filepath.Join(f.cwd, name)
		if err := os.Chmod(dir, 0o555); err != nil { //nolint:gosec // read-only fixture stays traversable while refusing writes
			t.Fatal(err)
		}
		t.Cleanup(func() { _ = os.Chmod(dir, 0o700) }) //nolint:gosec // restore owner traversal so TempDir cleanup removes the fixture
	}
}

func removePath(name string) fixtureStep {
	return func(t *testing.T, f *outcomeFixture) {
		t.Helper()
		if err := os.Remove(filepath.Join(f.cwd, name)); err != nil {
			t.Fatal(err)
		}
	}
}

func seedNested(t *testing.T, f *outcomeFixture) {
	t.Helper()
	writeFixture(t, f.cwd, "ro/f.txt", "ro-before\n")
	f.refs["refRO"] = seedReference(f.env, "ro/f.txt", "ro-before\n", true)
}

func failingDirSync(t *testing.T, _ *outcomeFixture) {
	t.Helper()
	calls := 0
	failDirectorySync(t, &calls)
}

type refusedThenStuckRunner struct {
	runner.Runner
	cwd    string
	aCalls int
}

func (r *refusedThenStuckRunner) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	switch path {
	case filepath.Join(r.cwd, "b.txt"):
		return fmt.Errorf("injected refusal: %w", fs.ErrPermission)
	case filepath.Join(r.cwd, "a.txt"):
		r.aCalls++
		if r.aCalls == 2 {
			return errors.New("injected restore failure")
		}
	}
	return r.Runner.WriteFile(ctx, path, data, perm)
}

var refusedThenStuck = wrapWith(func(run runner.Runner, cwd string) runner.Runner {
	return &refusedThenStuckRunner{Runner: run, cwd: cwd}
})

func TestCommitMachineryFailureIsInternal(t *testing.T) {
	tests := []failureCase{
		{name: "staging/profile", style: HashlineLight, steps: []fixtureStep{twoFiles, failStageWrite}, input: twoFileEdit},
		{name: "staging/legacy", style: Replace, steps: []fixtureStep{twoFiles, failStageWrite}, input: replaceTwoFiles},
		{name: "restored/profile", style: HashlineLight, steps: []fixtureStep{twoFiles, failSecondFile}, input: twoFileEdit},
		{name: "restored/legacy", style: Replace, steps: []fixtureStep{twoFiles, failSecondFile}, input: replaceTwoFiles},
		{name: "rollback/profile", style: HashlineLight, steps: []fixtureStep{twoFiles, failRestore}, input: twoFileEdit},
		{name: "rollback/legacy", style: Replace, steps: []fixtureStep{twoFiles, failRestore}, input: replaceTwoFiles},
		{name: "installed then failed/profile", style: HashlineLight, steps: []fixtureStep{twoFiles, installThenFail}, input: installOneEdit},
		{name: "directory sync/profile", style: HashlineLight, steps: []fixtureStep{twoFiles, failingDirSync}, input: installOneEdit},
		{name: "unknown/profile", style: HashlineLight, steps: []fixtureStep{twoFiles, blindAfterFailure}, input: twoFileEdit},
		{name: "rollback after a refused write/legacy", style: Replace, steps: []fixtureStep{twoFiles, refusedThenStuck}, input: replaceTwoFiles},
	}
	for _, test := range tests {
		test.want = tool.FailureInternal
		t.Run(test.name, test.run)
	}
}

func TestRequestedPathFailureStaysOrdinary(t *testing.T) {
	tests := []failureCase{
		{name: "target changed", style: HashlineLight, steps: []fixtureStep{twoFiles}, input: twoFileEdit, between: []fixtureStep{overwrite("a.txt", "a-drift\n")}},
		{name: "interrupted", style: HashlineLight, steps: []fixtureStep{twoFiles}, input: twoFileEdit, cancel: cancelAtApply},
		{name: "cancelled mid-commit/profile", style: HashlineLight, steps: []fixtureStep{twoFiles}, input: twoFileEdit, cancel: cancelMidCommit},
		{name: "cancelled mid-commit/legacy", style: Replace, steps: []fixtureStep{twoFiles}, input: replaceTwoFiles, cancel: cancelMidCommit},
		{name: "read-only directory", style: HashlineLight, steps: []fixtureStep{seedNested}, input: "[ro/f.txt@{refRO}]\nPUT 1:\n+ro-after\n", between: []fixtureStep{readOnlyDir("ro")}},
		{name: "read-only directory/legacy", style: Replace, steps: []fixtureStep{seedNested}, input: `{"changes":[{"path":"ro/f.txt","old":"ro-before","new":"ro-after"}]}`, between: []fixtureStep{readOnlyDir("ro")}},
		{name: "vanished directory", style: HashlineLight, steps: []fixtureStep{makeDir("gone")}, input: "[gone/n.txt#NEW]\nPUT >$:\n+x\n", between: []fixtureStep{removePath("gone")}},
		{name: "target became a directory", style: HashlineLight, steps: []fixtureStep{twoFiles}, input: installOneEdit, between: []fixtureStep{removePath("a.txt"), makeDir("a.txt")}},
	}
	for _, test := range tests {
		test.want = tool.FailureNone
		t.Run(test.name, test.run)
	}
}

func TestInvalidPatchStaysOrdinary(t *testing.T) {
	env, _ := editTestEnv(t)
	for _, style := range []Style{HashlineLight, Replace} {
		result, err := runPatch(t, env, style, "not a patch")
		if err == nil {
			t.Fatalf("%s accepted an invalid patch", style)
		}
		if result.Failure != tool.FailureNone {
			t.Fatalf("%s failure = %q, want an invalid patch to stay ordinary", style, result.Failure)
		}
	}
}

func TestCancelledPatchSyncFailureKeepsOutcomeAndIsNotReportable(t *testing.T) {
	f := newOutcomeFixture(t)
	twoFiles(t, f)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	stubDirectorySync(t, func(*dirPin) error {
		cancel()
		return errInjectedDirSync
	})
	input := f.input(installOneEdit)
	args := []byte(`{"input":` + snapshot.JSONString(input) + `}`)
	plan, err := New(string(HashlineLight)).Plan(t.Context(), tool.Call{ID: "call", Args: args}, boundEvidence(f.env, input))
	if err != nil {
		t.Fatal(err)
	}
	result, err := plan.Apply(ctx)
	if err == nil || !errors.Is(ctx.Err(), context.Canceled) || readFixture(t, f.cwd, "a.txt") != "a-after\n" {
		t.Fatalf("apply = %+v, %v; cancellation should leave the already-installed file", result, err)
	}
	requireSyncFailureText(t, err, "a.txt")
	if result.Failure != tool.FailureNone {
		t.Fatalf("failure class = %q; cancelled calls cannot be reported", result.Failure)
	}
}
