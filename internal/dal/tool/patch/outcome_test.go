package patch

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

const outcomeSource = "one\ntwo\nthree\n"

type outcomeFixture struct {
	env  tool.Env
	cwd  string
	refs map[string]string
}

type fixtureStep func(t *testing.T, f *outcomeFixture)

func newOutcomeFixture(t *testing.T) *outcomeFixture {
	t.Helper()
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", outcomeSource)
	writeFixture(t, cwd, "h.txt", outcomeSource)
	return &outcomeFixture{env: env, cwd: cwd, refs: map[string]string{
		"ref":     seedReference(env, "f.txt", outcomeSource, true),
		"ref2":    seedReference(env, "f.txt", outcomeSource, true),
		"refH":    seedReference(env, "h.txt", outcomeSource, true),
		"foreign": snapshot.New().Put("f.txt", []byte(outcomeSource), nil),
	}}
}

func (f *outcomeFixture) input(text string) string {
	for name, ref := range f.refs {
		text = strings.ReplaceAll(text, "{"+name+"}", ref)
	}
	return text
}

func (f *outcomeFixture) run(t *testing.T, style Style, text string) error {
	t.Helper()
	_, err := runPatch(t, f.env, style, f.input(text))
	return err
}

func (f *outcomeFixture) plan(t *testing.T, style Style, text string) (tool.Plan, error) {
	t.Helper()
	input := f.input(text)
	args := []byte(`{"input":` + snapshot.JSONString(input) + `}`)
	return New(string(style)).Plan(t.Context(), tool.Call{ID: "call", Args: args}, boundEvidence(f.env, input))
}

func overwrite(name, body string) fixtureStep {
	return func(t *testing.T, f *outcomeFixture) {
		t.Helper()
		writeFixture(t, f.cwd, name, body)
	}
}

func seedFile(name, body string, shown ...snapshot.Range) fixtureStep {
	return func(t *testing.T, f *outcomeFixture) {
		t.Helper()
		writeFixture(t, f.cwd, name, body)
		f.refs["ref"] = f.env.Snapshots.Put(name, []byte(body), shown)
	}
}

func deleteFile(name string) fixtureStep {
	return func(t *testing.T, f *outcomeFixture) {
		t.Helper()
		if err := os.Remove(filepath.Join(f.cwd, name)); err != nil {
			t.Fatal(err)
		}
	}
}

func makeDir(name string) fixtureStep {
	return func(t *testing.T, f *outcomeFixture) {
		t.Helper()
		if err := os.Mkdir(filepath.Join(f.cwd, name), 0o755); err != nil {
			t.Fatal(err)
		}
	}
}

func symlink(name, target string) fixtureStep {
	return func(t *testing.T, f *outcomeFixture) {
		t.Helper()
		if runtime.GOOS == "windows" {
			t.Skip("symlink creation requires additional privileges")
		}
		if err := os.Symlink(filepath.Join(f.cwd, target), filepath.Join(f.cwd, name)); err != nil {
			t.Fatal(err)
		}
	}
}

func wrapWith(wrap func(run runner.Runner, cwd string) runner.Runner) fixtureStep {
	return func(t *testing.T, f *outcomeFixture) {
		t.Helper()
		f.env.Runner = wrap(f.env.Runner, f.cwd)
	}
}

func twoFiles(t *testing.T, f *outcomeFixture) {
	t.Helper()
	writeFixture(t, f.cwd, "a.txt", "a-before\n")
	writeFixture(t, f.cwd, "b.txt", "b-before\n")
	f.refs["refA"] = seedReference(f.env, "a.txt", "a-before\n", true)
	f.refs["refB"] = seedReference(f.env, "b.txt", "b-before\n", true)
}

const twoFileEdit = "[a.txt@{refA}]\nPUT 1:\n+a-after\n[b.txt@{refB}]\nPUT 1:\n+b-after\n"

func maskSecrets(t *testing.T, f *outcomeFixture) {
	t.Helper()
	f.env.Masker = strictFailMasker{}
}

func bindElsewhere(t *testing.T, f *outcomeFixture) {
	t.Helper()
	f.env.Snapshots.Bind(f.refs["ref"], "another-consumer", 1)
	f.env.Evidence = snapshot.Evidence{Consumer: snapshot.ConsumerModel, Cutoff: 1}
}

func expireReferences(t *testing.T, f *outcomeFixture) {
	t.Helper()
	for range 512 {
		f.env.Snapshots.Put("x.txt", []byte("x\n"), nil)
	}
}

func forgetPath(t *testing.T, f *outcomeFixture) {
	t.Helper()
	f.env.Snapshots.Remove("f.txt", []byte(outcomeSource))
}

type failWriteRunner struct {
	runner.Runner
	target string
}

func (r failWriteRunner) WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error {
	if path == r.target {
		return errors.New("injected second-file failure")
	}
	return r.Runner.WriteFile(ctx, path, data, perm)
}

type failStatRunner struct {
	runner.Runner
	target string
}

func (r failStatRunner) Stat(ctx context.Context, path string) (runner.FileInfo, error) {
	if path == r.target {
		return runner.FileInfo{}, errors.New("injected stat failure")
	}
	return r.Runner.Stat(ctx, path)
}

type plainRunner struct{ runner.Runner }

var (
	failSecondFile = wrapWith(func(run runner.Runner, cwd string) runner.Runner {
		return failWriteRunner{Runner: run, target: filepath.Join(cwd, "b.txt")}
	})
	failRestore = wrapWith(func(run runner.Runner, cwd string) runner.Runner {
		return &failRestoreRunner{Runner: run, cwd: cwd}
	})
	failStageWrite = wrapWith(func(run runner.Runner, _ string) runner.Runner { return failStageRunner{Runner: run} })
	withoutLinks   = wrapWith(func(run runner.Runner, _ string) runner.Runner { return plainRunner{Runner: run} })
	failTargetStat = wrapWith(func(run runner.Runner, cwd string) runner.Runner {
		return failStatRunner{Runner: run, target: filepath.Join(cwd, "f.txt")}
	})
)

type outcomeCase struct {
	name     string
	styles   []Style
	steps    []fixtureStep
	input    string
	apply    bool
	between  fixtureStep
	run      func(t *testing.T, style Style) error
	direct   bool
	suffix   string
	contains string
	want     tool.Outcome
}

var (
	profileStyles = []Style{HashlineLight, HashlineEnhanced}
	lightOnly     = []Style{HashlineLight}
	enhancedOnly  = []Style{HashlineEnhanced}
)

func refused(status tool.Status, reason tool.Reason, class tool.ErrorClass) tool.Outcome {
	return tool.Outcome{Status: status, Commit: tool.CommitNone, Reason: reason, Class: class}
}

func rejected(reason tool.Reason, class tool.ErrorClass) tool.Outcome {
	return refused(tool.StatusRejected, reason, class)
}

func writeFailure(commit tool.CommitState, reason tool.Reason) tool.Outcome {
	return tool.Outcome{Status: tool.StatusWriteError, Commit: commit, Reason: reason, Class: tool.ErrClassIO}
}

var commitUnknown = tool.Outcome{
	Status: tool.StatusIndeterminate, Commit: tool.CommitUnknown, Reason: tool.ReasonCommitUnknown, Class: tool.ErrClassIO,
}

func editCase(name, input string, want tool.Outcome, steps ...fixtureStep) outcomeCase {
	return outcomeCase{name: name, input: input, want: want, steps: steps}
}

func (c outcomeCase) execute(t *testing.T, style Style) error {
	t.Helper()
	if c.run != nil {
		return c.run(t, style)
	}
	f := newOutcomeFixture(t)
	for _, step := range c.steps {
		step(t, f)
	}
	if !c.apply {
		return f.run(t, style, c.input)
	}
	plan, err := f.plan(t, style, c.input)
	if err != nil {
		t.Fatalf("plan: %v", err)
	}
	if c.between != nil {
		c.between(t, f)
	}
	_, err = plan.Apply(t.Context())
	return err
}

func runOutcomeCases(t *testing.T, cases []outcomeCase) {
	t.Helper()
	for _, item := range cases {
		styles := item.styles
		if len(styles) == 0 {
			styles = profileStyles
		}
		if item.direct {
			styles = styles[:1]
		}
		for _, style := range styles {
			t.Run(item.name+"/"+string(style), func(t *testing.T) {
				requireCase(t, item, style, item.execute(t, style))
			})
		}
	}
}

func requireCase(t *testing.T, item outcomeCase, style Style, err error) {
	t.Helper()
	if err == nil {
		t.Fatal("the call succeeded")
	}
	if item.direct {
		requireOutcomeValue(t, outcomeOf(err), item.want, err)
	} else {
		requireWrappedOutcome(t, err, style, item.want)
	}
	text := err.Error()
	if !strings.HasSuffix(text, item.suffix) || !strings.Contains(text, item.contains) {
		t.Fatalf("error %q lacks suffix %q or text %q", text, item.suffix, item.contains)
	}
}

func requireOutcomeValue(t *testing.T, got, want tool.Outcome, err error) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("outcome %+v, want %+v (error %q)", got, want, err)
	}
}

func requireWrappedOutcome(t *testing.T, err error, style Style, want tool.Outcome) {
	t.Helper()
	var failure *tool.OutcomeError
	if !errors.As(err, &failure) {
		t.Fatalf("error %q carries no outcome", err)
	}
	want.Style, want.Version = string(style), 1
	requireOutcomeValue(t, failure.Outcome, want, err)
	if failure.Err.Error() != err.Error() {
		t.Fatalf("outcome error text %q differs from the cause %q", err, failure.Err)
	}
	if invalid := failure.Outcome.Valid(); invalid != nil {
		t.Fatalf("outcome is not a C13 outcome: %v", invalid)
	}
}

func cancelBeforeApply(t *testing.T, style Style) error {
	f := newOutcomeFixture(t)
	plan, err := f.plan(t, style, "[f.txt@{ref}]\nPUT 1:\n+uno\n")
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	_, err = plan.Apply(ctx)
	if actual := readFixture(t, f.cwd, "f.txt"); actual != outcomeSource {
		t.Fatalf("cancelled call changed the target to %q", actual)
	}
	return err
}

func cancelAfterFirstWrite(t *testing.T, style Style) error {
	f := newOutcomeFixture(t)
	twoFiles(t, f)
	ctx, cancel := context.WithCancel(t.Context())
	defer cancel()
	f.env.Runner = &cancelAfterFirstRunner{Runner: f.env.Runner, cwd: f.cwd, cancel: cancel}
	input := f.input(twoFileEdit)
	args := jsontext.Value(`{"input":` + snapshot.JSONString(input) + `}`)
	plan, err := New(string(style)).Plan(ctx, tool.Call{ID: "call", Args: args}, boundEvidence(f.env, input))
	if err != nil {
		t.Fatal(err)
	}
	_, err = plan.Apply(ctx)
	return err
}

func insertThenCut(t *testing.T, style Style) error {
	f := newOutcomeFixture(t)
	if err := f.run(t, style, "[f.txt@{ref}]\nPUT <3:\n+inside\n"); err != nil {
		t.Fatalf("recorded insertion: %v", err)
	}
	return f.run(t, style, "[f.txt@{ref}]\nCUT 2-3\n")
}

func appendTwice(t *testing.T, style Style) error {
	f := newOutcomeFixture(t)
	if err := f.run(t, style, "[f.txt@{ref}]\nPUT >$:\n+four\n"); err != nil {
		t.Fatalf("first append: %v", err)
	}
	return f.run(t, style, "[f.txt@{ref}]\nPUT >$:\n+five\n")
}

func TestOutcomeStatusAndCommitState(t *testing.T) {
	cases := []outcomeCase{
		{
			name: "L-02", styles: lightOnly, input: "[f.txt@{ref}]\nPUT 1:\n+uno\n",
			steps:  []fixtureStep{overwrite("f.txt", "one\nTWO\nthree\n")},
			suffix: "is stale: snapshot: stale reference. Read again",
			want:   refused(tool.StatusNeedsContext, tool.ReasonHistoryGap, tool.ErrClassStale),
		},
		{
			name: "L-04", styles: lightOnly, run: insertThenCut,
			suffix: "is stale: snapshot: target changed. Read again",
			want:   refused(tool.StatusConflict, tool.ReasonTargetChanged, tool.ErrClassStale),
		},
		{
			name: "E-09", styles: enhancedOnly, run: appendTwice,
			suffix: "is stale: snapshot: gap changed. Read again",
			want:   refused(tool.StatusConflict, tool.ReasonGapChanged, tool.ErrClassStale),
		},
		{
			name: "BATCH-02", input: "[f.txt@{ref}]\nPUT 1:\n+x\n[f.txt@{ref2}]\nPUT 3:\n+y\n",
			contains: "patch: mixed_references for f.txt",
			want:     rejected(tool.ReasonMixedReferences, tool.ErrClassResolve),
		},
		{
			name: "FILE-05/another section", input: "[f.txt@{ref}]\nMV g.txt\n[f.txt@{ref}]\nPUT 1:\n+x\n",
			contains: "patch: rename must be the last operation for f.txt",
			want:     rejected(tool.ReasonInvalidShape, tool.ErrClassFile),
		},
		{
			name: "FILE-05/same section", input: "[f.txt@{ref}]\nMV g.txt\nPUT 1:\n+x\n",
			contains: "expected a new file section",
			want:     rejected(tool.ReasonInvalidShape, tool.ErrClassFile),
		},
		{
			name: "IO-02", styles: lightOnly, input: twoFileEdit,
			steps:    []fixtureStep{twoFiles, failSecondFile},
			contains: "All completed writes were restored",
			want:     withPaths(writeFailure(tool.CommitRestored, tool.ReasonWriteFailed), "a.txt", "before", "b.txt", "before"),
		},
		{
			name: "IO-03", styles: lightOnly, input: twoFileEdit,
			steps:    []fixtureStep{twoFiles, failRestore},
			contains: "Rollback was incomplete",
			want:     withPaths(writeFailure(tool.CommitPartial, tool.ReasonRollbackFailed), "a.txt", "after", "b.txt", "before"),
		},
		{
			name: "IO-04", styles: lightOnly, input: twoFileEdit,
			steps:    []fixtureStep{twoFiles, blindAfterFailure},
			contains: "Retained backup for a.txt",
			want:     withPaths(commitUnknown, "a.txt", "unknown", "b.txt", "before"),
		},
		{
			name: "IO-05", styles: lightOnly, input: "[a.txt@{refA}]\nPUT 1:\n+a-after\n",
			steps:    []fixtureStep{twoFiles, installThenFail},
			contains: "injected sync failure",
			want:     withPaths(writeFailure(tool.CommitComplete, tool.ReasonSyncFailed), "a.txt", "after"),
		},
		{
			name: "IO-07", styles: lightOnly, run: cancelBeforeApply,
			contains: "context canceled",
			want:     refused(tool.StatusInterrupted, tool.ReasonInterrupted, tool.ErrClassNone),
		},
		{
			name: "IO-08", styles: lightOnly, run: cancelAfterFirstWrite,
			contains: "All completed writes were restored",
			want:     withPaths(writeFailure(tool.CommitRestored, tool.ReasonWriteFailed), "a.txt", "before", "b.txt", "before"),
		},
	}
	runOutcomeCases(t, cases)
}

func pathStates(pairs ...string) []tool.PathState {
	out := make([]tool.PathState, 0, len(pairs)/2)
	for pair := range slices.Chunk(pairs, 2) {
		out = append(out, tool.PathState{Path: pair[0], State: pair[1]})
	}
	return out
}

func withPaths(outcome tool.Outcome, pairs ...string) tool.Outcome {
	outcome.Paths = pathStates(pairs...)
	return outcome
}

func TestVerifiedCommitFailureOutcomeRule(t *testing.T) {
	boom := errors.New("boom")
	none := writeFailure(tool.CommitNone, tool.ReasonWriteFailed)
	restored := writeFailure(tool.CommitRestored, tool.ReasonWriteFailed)
	rollbackFailed := writeFailure(tool.CommitPartial, tool.ReasonRollbackFailed)
	syncFailed := writeFailure(tool.CommitPartial, tool.ReasonSyncFailed)
	complete := writeFailure(tool.CommitComplete, tool.ReasonSyncFailed)
	interrupted := refused(tool.StatusInterrupted, tool.ReasonInterrupted, tool.ErrClassNone)
	tests := []struct {
		row      string
		name     string
		cause    error
		paths    []tool.PathState
		effects  int
		failures int
		want     tool.Outcome
	}{
		{"IO-02", "no effect", boom, pathStates("a", "before"), 0, 0, withPaths(none, "a", "before")},
		{"IO-02", "no effect with a classified cause", reasoned(tool.ReasonNoReplaceUnavailable, boom), pathStates("a", "before"), 0, 0, withPaths(none, "a", "before")},
		{"IO-02", "restored", boom, pathStates("a", "before", "b", "before"), 2, 0, withPaths(restored, "a", "before", "b", "before")},
		{"IO-02", "restore error but the bytes are back", boom, pathStates("a", "before"), 1, 1, withPaths(restored, "a", "before")},
		{"IO-03", "rollback failed", boom, pathStates("a", "after", "b", "before"), 2, 1, withPaths(rollbackFailed, "a", "after", "b", "before")},
		{"IO-03", "mix without a rollback failure", boom, pathStates("a", "after", "b", "before"), 1, 0, withPaths(syncFailed, "a", "after", "b", "before")},
		{"IO-04", "one path unknown", boom, pathStates("a", "unknown", "b", "before"), 1, 0, withPaths(commitUnknown, "a", "unknown", "b", "before")},
		{"IO-04", "unknown beats a mix", boom, pathStates("a", "after", "b", "unknown"), 1, 1, withPaths(commitUnknown, "a", "after", "b", "unknown")},
		{"IO-05", "every path installed", boom, pathStates("a", "after"), 0, 0, withPaths(complete, "a", "after")},
		{"IO-05", "two paths installed", boom, pathStates("a", "after", "b", "after"), 1, 0, withPaths(complete, "a", "after", "b", "after")},
		{"IO-07", "cancelled before an effect", context.Canceled, pathStates("a", "before"), 0, 0, withPaths(interrupted, "a", "before")},
		{"IO-07", "deadline before an effect", context.DeadlineExceeded, pathStates("a", "before"), 0, 0, withPaths(interrupted, "a", "before")},
		{"IO-07", "cancelled but a path reads after", context.Canceled, pathStates("a", "after"), 0, 0, withPaths(complete, "a", "after")},
		{"IO-08", "cancelled after an effect", context.Canceled, pathStates("a", "before", "b", "before"), 1, 0, withPaths(restored, "a", "before", "b", "before")},
		{"IO-08", "cancelled and rollback failed", context.Canceled, pathStates("a", "after", "b", "before"), 1, 1, withPaths(rollbackFailed, "a", "after", "b", "before")},
	}
	for _, test := range tests {
		t.Run(test.row+"/"+test.name, func(t *testing.T) {
			got := failedCommit(test.cause, test.paths, test.effects, test.failures)
			requireOutcomeValue(t, got, test.want, test.cause)
			if err := got.Valid(); err != nil {
				t.Fatal(err)
			}
		})
	}
}

func TestEveryProfileRefusalCarriesAnOutcome(t *testing.T) {
	var cases []outcomeCase
	for _, group := range [][]outcomeCase{
		syntaxCases(), shapeCases(), overlapCases(), limitCases(), fileCases(),
		proofCases(), conflictCases(), commitCases(),
	} {
		cases = append(cases, group...)
	}
	runOutcomeCases(t, cases)
}

func syntaxCases() []outcomeCase {
	syntax := rejected(tool.ReasonInvalidSyntax, tool.ErrClassParse)
	blockOp := rejected(tool.ReasonInvalidSyntax, tool.ErrClassResolve)
	blockText := "patch: line 2: block ops (N*) are not supported; use a line range"
	badRange := rejected(tool.ReasonInvalidRange, tool.ErrClassResolve)
	return []outcomeCase{
		{name: "C15/payload that is not a string or an object", run: numberPayload, contains: "patch: invalid input", want: syntax},
		editCase("C15/no header", "PUT 1:\n+x\n", syntax),
		editCase("C04/tag header", "[f.txt#ABCD]\nPUT 1:\n+x\n", syntax),
		editCase("C15/no trailing colon", "[f.txt@{ref}]\nPUT 1\n+x\n", syntax),
		{name: "L-05 E-10/PUT block op", input: "[f.txt@{ref}]\nPUT 2*:\n+x\n", contains: blockText, want: blockOp},
		{name: "L-05 E-10/other block op", input: "[f.txt@{ref}]\nFOO 2*\n", contains: blockText, want: blockOp},
		editCase("C15/range zero", "[f.txt@{ref}]\nPUT 0:\n+x\n", badRange),
		editCase("C15/range reversed", "[f.txt@{ref}]\nCUT 3-1\n", badRange),
		editCase("C15/gap number", "[f.txt@{ref}]\nPUT >x:\n+y\n", badRange),
		editCase("C15/range past the end", "[f.txt@{ref}]\nPUT 9:\n+x\n", badRange),
		editCase("C15/gap past the end", "[f.txt@{ref}]\nPUT >9:\n+x\n", badRange),
	}
}

func numberPayload(t *testing.T, style Style) error {
	f := newOutcomeFixture(t)
	_, err := New(string(style)).Run(t.Context(), tool.Call{ID: "call", Args: jsontext.Value("123")}, f.env)
	return err
}

func shapeCases() []outcomeCase {
	shape := rejected(tool.ReasonInvalidShape, tool.ErrClassFile)
	return []outcomeCase{
		editCase("C15/REM with another operation", "[f.txt@{ref}]\nREM\nPUT 1:\n+x\n", shape),
		editCase("C15/new file with a range", "[g.txt#NEW]\nPUT 1:\n+x\n", shape),
		editCase("C15/new file with CUT", "[g.txt#NEW]\nCUT 1\n", shape),
		editCase("C15/new file with REM", "[g.txt#NEW]\nREM\n", shape),
		editCase("C15/new file with MV", "[g.txt#NEW]\nMV h2.txt\n", shape),
		editCase("C15/new file with two rows", "[g.txt#NEW]\nPUT >$:\n+a\nPUT >$:\n+b\n", shape),
		editCase("C15/two creates of one path", "[g.txt#NEW]\nPUT >$:\n+a\n[g.txt#NEW]\nPUT >$:\n+b\n", shape),
		editCase("C15/two removals of one path", "[f.txt@{ref}]\nREM\n[f.txt@{ref}]\nREM\n", shape),
		{name: "C15/whole file operation with another", direct: true, run: wholeFileTwice, contains: "operations for f.txt cannot combine", want: shape},
		{name: "C15/symbolic link refusal of a pin", direct: true, run: pinLinkRefusal, want: shape},
		{name: "C15/empty path", direct: true, run: emptyPath, want: shape},
		{name: "C15/binary target", input: "[b.bin@{ref}]\nPUT 1:\n+x\n", steps: []fixtureStep{overwrite("b.bin", "a\x00b\n")}, contains: "is not a text file", want: shape},
		{name: "C15/directory target", input: "[d@{ref}]\nPUT 1:\n+x\n", steps: []fixtureStep{makeDir("d")}, contains: "is not a regular file", want: shape},
		{name: "C15/symbolic link in the path", input: "[l.txt@{ref}]\nPUT 1:\n+x\n", steps: []fixtureStep{symlink("l.txt", "f.txt")}, contains: "contains a symbolic link", want: shape},
	}
}

func wholeFileTwice(_ *testing.T, _ Style) error {
	whole := edit{kind: editChange, locator: locator{kind: locatorWhole}}
	return validateChangeShapes(fileGroup{display: "f.txt", edits: []edit{whole, whole}})
}

func pinLinkRefusal(_ *testing.T, _ Style) error { return symlinkRefusal("d/f.txt") }

func emptyPath(t *testing.T, _ Style) error {
	_, _, err := resolvePatchPath(t.TempDir(), "")
	return err
}

func overlapCases() []outcomeCase {
	overlap := rejected(tool.ReasonOverlap, tool.ErrClassResolve)
	return []outcomeCase{
		{name: "C15/overlapping ranges", input: "[f.txt@{ref}]\nPUT 1-2:\n+a\nPUT 2:\n+b\n", contains: "overlap in f.txt", want: overlap},
		{name: "BATCH-04/one gap twice", input: "[f.txt@{ref}]\nPUT <2:\n+a\nPUT <2:\n+b\n", contains: "conflicting insertion gaps in f.txt", want: overlap},
		{name: "C15/two renames to one destination", input: "[f.txt@{ref}]\nMV g.txt\n[h.txt@{refH}]\nMV g.txt\n", contains: "another change also writes g.txt", want: overlap},
		{name: "C15/rename onto a changed file", input: "[f.txt@{ref}]\nMV h.txt\n[h.txt@{refH}]\nPUT 1:\n+x\n", contains: "another change also writes h.txt", want: overlap},
		{name: "C15/rename claims over two staged files", direct: true, run: renameClaims, contains: "another change also writes b.txt", want: overlap},
	}
}

func renameClaims(_ *testing.T, _ Style) error {
	return checkRenameTargets([]*stagedFile{
		{path: "a.txt", absolute: "/w/a.txt", renameTo: "b.txt", renameAbs: "/w/b.txt"},
		{path: "b.txt", absolute: "/w/b.txt"},
	})
}

func limitCases() []outcomeCase {
	limit := rejected(tool.ReasonResourceLimit, tool.ErrClassLimit)
	return []outcomeCase{
		editCase("C14/over 64 operations", "[f.txt@{ref}]\n"+strings.Repeat("CUT 1\n", 65), limit),
		{name: "C14/staged bytes", direct: true, run: stagedBytes, want: limit},
		{name: "C14/files", direct: true, run: sixtyFiveFiles, contains: "the limit is 64", want: limit},
	}
}

func stagedBytes(_ *testing.T, _ Style) error { return checkStagedBytes(64<<20 + 1) }

func sixtyFiveFiles(t *testing.T, _ Style) error {
	f := newOutcomeFixture(t)
	items := make([]edit, 65)
	for i := range items {
		items[i] = edit{kind: editCreate, path: fmt.Sprintf("n%d.txt", i)}
	}
	_, err := checkedGroups(t.Context(), f.env, items)
	return err
}

func fileCases() []outcomeCase {
	outside := rejected(tool.ReasonOutsideWorkspace, tool.ErrClassFile)
	unavailable := rejected(tool.ReasonObservationUnavailable, tool.ErrClassBlocked)
	goSource := seedFile("x.go", "package a\n\nfunc f() {\n\treturn\n}\n", snapshot.Range{Start: 1, End: 5})
	placeholder := "\t// " + omissionDots() + " existing code " + omissionDots() + "\n"
	return []outcomeCase{
		editCase("C15/parent path", "[../x.txt@{ref}]\nPUT 1:\n+x\n", outside),
		editCase("C15/rooted path", "[/etc/x.txt@{ref}]\nPUT 1:\n+x\n", outside),
		editCase("C15/rename out of the workspace", "[f.txt@{ref}]\nMV ../x.txt\n", outside),
		{name: "C15/rename with a parent component", input: "[f.txt@{ref}]\nMV sub/../x.txt\n", contains: "rename destination contains parent traversal", want: outside},
		{name: "C15/rename with a parent component at staging", direct: true, run: stageParentRename, contains: "rename destination contains parent traversal", want: outside},
		{name: "C15/secret placeholder", input: "[f.txt@{ref}]\nPUT 1:\n+{{secret:pattern0:0}}\n", steps: []fixtureStep{maskSecrets}, contains: "cannot be restored", want: rejected(tool.ReasonPermissionDenied, tool.ErrClassBlocked)},
		{name: "C15/observer block", input: "[x.go@{ref}]\nPUT 4:\n+" + placeholder, steps: []fixtureStep{goSource}, contains: "stands in for omitted code", want: rejected(tool.ReasonObserverBlocked, tool.ErrClassBlocked)},
		{name: "C15/reference span with no store", direct: true, run: spanWithoutStore, want: unavailable},
		{name: "C15/reference gap with no store", direct: true, run: gapWithoutStore, want: unavailable},
	}
}

func stageParentRename(t *testing.T, _ Style) error {
	f := newOutcomeFixture(t)
	resolver := fileResolver{env: f.env, group: fileGroup{display: "f.txt"}, file: &stagedFile{}}
	return resolver.stageRename(t.Context(), edit{kind: editRename, to: "sub/../x.txt"})
}

func spanWithoutStore(_ *testing.T, _ Style) error {
	_, err := mapReferenceSpan(nil, "r", &stagedFile{path: "f.txt"}, 1, 1)
	return err
}

func gapWithoutStore(_ *testing.T, _ Style) error {
	_, err := mapReferenceGap(nil, "r", &stagedFile{path: "f.txt"}, 1)
	return err
}

func proofCases() []outcomeCase {
	partial := seedFile("f.txt", outcomeSource, snapshot.Range{Start: 1, End: 1})
	unterminated := seedFile("f.txt", "one\ntwo\nthree", snapshot.Range{Start: 1, End: 2})
	unobserved := refused(tool.StatusNeedsContext, tool.ReasonUnobservedRange, tool.ErrClassProof)
	needsRead := "need prior observation. Read them, then resend"
	return []outcomeCase{
		{name: "C15/binding mismatch", input: "[f.txt@{ref}]\nPUT 1:\n+x\n", steps: []fixtureStep{bindElsewhere}, contains: "snapshot: reference binding mismatch", want: rejected(tool.ReasonBindingMismatch, tool.ErrClassProof)},
		{name: "C15/unknown reference", input: "[f.txt@{foreign}]\nPUT 1:\n+x\n", contains: "snapshot: unknown reference", want: refused(tool.StatusNeedsContext, tool.ReasonReferenceUnknown, tool.ErrClassProof)},
		{name: "C15/expired reference", input: "[f.txt@{ref}]\nPUT 1:\n+x\n", steps: []fixtureStep{expireReferences}, contains: "snapshot: expired reference", want: refused(tool.StatusNeedsContext, tool.ReasonReferenceExpired, tool.ErrClassProof)},
		{name: "C15/unobserved range", styles: enhancedOnly, input: "[f.txt@{ref}]\nPUT 2:\n+x\n", steps: []fixtureStep{partial}, suffix: needsRead, want: unobserved},
		{name: "C15/unobserved final row", styles: enhancedOnly, input: "[f.txt@{ref}]\nPUT >3:\n+x\n", steps: []fixtureStep{unterminated}, suffix: needsRead, want: unobserved},
		{name: "C15/removed path", input: "[f.txt@{ref}]\nPUT 1:\n+x\n", steps: []fixtureStep{forgetPath}, suffix: "is stale: snapshot: path changed. Read again", want: refused(tool.StatusConflict, tool.ReasonPathChanged, tool.ErrClassStale)},
	}
}

func conflictCases() []outcomeCase {
	pathChanged := refused(tool.StatusConflict, tool.ReasonPathChanged, tool.ErrClassStale)
	targetChanged := refused(tool.StatusConflict, tool.ReasonTargetChanged, tool.ErrClassStale)
	drift := overwrite("f.txt", "one\nTWO\nthree\n")
	return []outcomeCase{
		editCase("C15/new file over an existing one", "[h.txt#NEW]\nPUT >$:\n+x\n", pathChanged),
		editCase("C15/rename onto an existing file", "[f.txt@{ref}]\nMV h.txt\n", pathChanged),
		{name: "C15/target removed after the read", input: "[f.txt@{ref}]\nPUT 1:\n+x\n", steps: []fixtureStep{deleteFile("f.txt")}, contains: "patch: f.txt could not be read", want: pathChanged},
		{name: "C15/REM after the file changed", input: "[f.txt@{ref}]\nREM\n", steps: []fixtureStep{drift}, suffix: "is expired or the file changed. Read again", want: targetChanged},
		{name: "C15/MV after the file changed", input: "[f.txt@{ref}]\nMV g.txt\n", steps: []fixtureStep{drift}, suffix: "is expired or the file changed. Read again", want: targetChanged},
	}
}

func commitCases() []outcomeCase {
	pathChanged := refused(tool.StatusConflict, tool.ReasonPathChanged, tool.ErrClassStale)
	noReplace := rejected(tool.ReasonNoReplaceUnavailable, tool.ErrClassFile)
	writeFailed := writeFailure(tool.CommitNone, tool.ReasonWriteFailed)
	createG := "[g.txt#NEW]\nPUT >$:\n+x\n"
	editF := "[f.txt@{ref}]\nPUT 1:\n+uno\n"
	takeG := overwrite("g.txt", "taken\n")
	return []outcomeCase{
		{name: "C15/target cannot be inspected", input: editF, steps: []fixtureStep{failTargetStat}, contains: "injected stat failure", want: writeFailed},
		{name: "C12/target changed while approving", input: editF, apply: true, between: overwrite("f.txt", "one\ntwo\nthree\nfour\n"), suffix: "patch: f.txt changed while you were approving. Nothing was written. Try again", want: refused(tool.StatusConflict, tool.ReasonTargetChanged, tool.ErrClassStale)},
		{name: "C12/path appeared while approving", input: createG, apply: true, between: takeG, suffix: "patch: g.txt already exists", want: pathChanged},
		{name: "C12/path appeared while approving without a pinned directory", input: createG, steps: []fixtureStep{withoutLinks}, apply: true, between: takeG, suffix: "patch: g.txt already exists", want: pathChanged},
		{name: "C13/no replace writer", input: createG, steps: []fixtureStep{withoutLinks}, apply: true, contains: "no_replace_unavailable", want: noReplace},
		{name: "C13/no replace publish", direct: true, run: publishWithoutLinks, contains: "no_replace_unavailable", want: noReplace},
		{name: "C13/staging failure", input: editF, steps: []fixtureStep{failStageWrite}, apply: true, contains: "injected staging failure", want: writeFailed},
	}
}

func publishWithoutLinks(t *testing.T, _ Style) error {
	f := newOutcomeFixture(t)
	env := tool.Env{Runner: plainRunner{Runner: f.env.Runner}, Cwd: f.cwd}
	return publishAbsent(t.Context(), env, &writeState{}, filepath.Join(f.cwd, "g.txt"))
}

func TestEnhancedWithoutDeliveryBoundaryIsObservationUnavailable(t *testing.T) {
	t.Run("OBS-12", func(t *testing.T) {
		f := newOutcomeFixture(t)
		input := f.input("[f.txt@{ref}]\nPUT 1:\n+uno\n")
		args := []byte(`{"input":` + snapshot.JSONString(input) + `}`)
		_, err := New(string(HashlineEnhanced)).Run(t.Context(), tool.Call{ID: "call", Args: args}, f.env)
		want := rejected(tool.ReasonObservationUnavailable, tool.ErrClassBlocked)
		requireCase(t, outcomeCase{contains: "snapshot: no delivery boundary", want: want}, HashlineEnhanced, err)
		if actual := readFixture(t, f.cwd, "f.txt"); actual != outcomeSource {
			t.Fatalf("refused call changed the target to %q", actual)
		}
	})
}

func stagedCommit(t *testing.T, input string) (tool.Env, stagedBatch) {
	t.Helper()
	f := newOutcomeFixture(t)
	text := f.input(input)
	env := boundEvidence(f.env, text)
	parsed, err := parse(HashlineLight, text)
	if err != nil {
		t.Fatal(err)
	}
	batch, err := stage(t.Context(), HashlineLight, parsed, env, text)
	if err != nil {
		t.Fatal(err)
	}
	return env, batch
}

func TestCommitBatchReportsItsOutcome(t *testing.T) {
	t.Run("C13/applied", func(t *testing.T) {
		env, batch := stagedCommit(t, "[f.txt@{ref}]\nPUT 1:\n+uno\n")
		result, outcome, err := commitBatch(t.Context(), env, batch)
		if err != nil || !result.OK {
			t.Fatalf("commit = %+v, %v", result, err)
		}
		requireOutcomeValue(t, outcome, tool.Outcome{Status: tool.StatusApplied, Commit: tool.CommitComplete}, err)
		if invalid := outcome.Valid(); invalid != nil {
			t.Fatal(invalid)
		}
	})
	t.Run("C13/no_change", func(t *testing.T) {
		env, batch := stagedCommit(t, "[f.txt@{ref}]\nPUT 1:\n+one\n")
		result, outcome, err := commitBatch(t.Context(), env, batch)
		if err != nil || string(result.Output) != "no_change\n" {
			t.Fatalf("commit = %+v, %v", result, err)
		}
		requireOutcomeValue(t, outcome, tool.Outcome{Status: tool.StatusNoChange, Commit: tool.CommitNone}, err)
		if invalid := outcome.Valid(); invalid != nil {
			t.Fatal(invalid)
		}
	})
}

func requireNoOutcome(t *testing.T, err error) {
	t.Helper()
	if err == nil {
		t.Fatal("the call succeeded")
	}
	var failure *tool.OutcomeError
	if errors.As(err, &failure) {
		t.Fatalf("legacy error %q carries the outcome %+v", err, failure.Outcome)
	}
}

func legacyPlan(t *testing.T, style Style, drift bool) (tool.Plan, string, error) {
	t.Helper()
	const source = "one\ntwo\n"
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", source)
	proof := styleProofFor(env, style, source)
	if drift {
		writeFixture(t, cwd, "f.txt", "drift\n")
	}
	input := renderStyle(style, []string{"one", "two"}, []referenceEdit{{start: 0, end: 1, body: []string{"new"}}}, proof)
	plan, err := New(string(style)).Plan(t.Context(), tool.Call{ID: "call", Args: styleArgs(style, input)}, boundEvidence(env, input))
	return plan, cwd, err
}

func TestLegacyStylesCarryNoOutcome(t *testing.T) {
	for _, style := range []Style{Anchor, Replace, Hashline, ApplyPatch} {
		name := "COMPAT-01/" + string(style)
		t.Run(name+"/applied", func(t *testing.T) { requireLegacyApplied(t, style) })
		t.Run(name+"/refused plan", func(t *testing.T) { requireLegacyPlanRefused(t, style) })
		t.Run(name+"/refused apply", func(t *testing.T) { requireLegacyApplyRefused(t, style) })
	}
	t.Run("COMPAT-01/rewrite", requireRewriteCarriesNoOutcome)
}

func requireLegacyApplied(t *testing.T, style Style) {
	plan, _, err := legacyPlan(t, style, false)
	if err != nil {
		t.Fatal(err)
	}
	result, err := plan.Apply(t.Context())
	if err != nil || len(result.Meta) != 0 {
		t.Fatalf("apply = %+v, %v", result, err)
	}
}

func requireLegacyPlanRefused(t *testing.T, style Style) {
	_, _, err := legacyPlan(t, style, true)
	requireNoOutcome(t, err)
}

func requireLegacyApplyRefused(t *testing.T, style Style) {
	plan, cwd, err := legacyPlan(t, style, false)
	if err != nil {
		t.Fatal(err)
	}
	writeFixture(t, cwd, "f.txt", "drift\n")
	_, err = plan.Apply(t.Context())
	requireNoOutcome(t, err)
}

func requireRewriteCarriesNoOutcome(t *testing.T) {
	const source = "one\ntwo\n"
	replaced := func(before string) []ReplacedFile {
		return []ReplacedFile{{Path: "f.txt", Before: []byte(before), After: []byte("new\ntwo\n")}}
	}
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", source)
	_, err := PlanReplace(t.Context(), env, "summary\n", replaced("stale\n"))
	requireNoOutcome(t, err)
	plan, err := PlanReplace(t.Context(), env, "summary\n", replaced(source))
	if err != nil {
		t.Fatal(err)
	}
	writeFixture(t, cwd, "f.txt", "drift\n")
	_, err = plan.Apply(t.Context())
	requireNoOutcome(t, err)
	writeFixture(t, cwd, "f.txt", source)
	plan, err = PlanReplace(t.Context(), env, "summary\n", replaced(source))
	if err != nil {
		t.Fatal(err)
	}
	if result, applyErr := plan.Apply(t.Context()); applyErr != nil || len(result.Meta) != 0 {
		t.Fatalf("apply = %+v, %v", result, applyErr)
	}
}
