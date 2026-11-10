package patch

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func profileInput(path, ref, operations string) string {
	return "[" + path + "@" + ref + "]\n" + operations
}

func TestHashlineLineageMapsOriginalTargets(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\nthree\nfour\n"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	first := profileInput("f.txt", ref, "PUT <2:\n+inserted\n")
	if _, err := runPatch(t, env, HashlineLight, first); err != nil {
		t.Fatal(err)
	}
	second := profileInput("f.txt", ref, "PUT 4:\n+FOUR\n")
	if _, err := runPatch(t, env, HashlineEnhanced, second); err != nil {
		t.Fatal(err)
	}
	expected := "one\ninserted\ntwo\nthree\nFOUR\n"
	if actual := readFixture(t, cwd, "f.txt"); actual != expected {
		t.Fatalf("mapped edit %q, want %q", actual, expected)
	}
}

func TestHashlineGapLineage(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\nthree\n"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", ref, "PUT <2:\n+inserted\n")); err != nil {
		t.Fatal(err)
	}
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", ref, "PUT <3:\n+before-original-three\n")); err != nil {
		t.Fatal(err)
	}
	expected := "one\ninserted\ntwo\nbefore-original-three\nthree\n"
	if actual := readFixture(t, cwd, "f.txt"); actual != expected {
		t.Fatalf("mapped gap %q, want %q", actual, expected)
	}
}

func TestHashlineConflictIsSticky(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\nthree\n"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", ref, "PUT <3:\n+inside\n")); err != nil {
		t.Fatal(err)
	}
	before := readFixture(t, cwd, "f.txt")
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", ref, "CUT 2-3")); err == nil {
		t.Fatal("interior insertion did not conflict")
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != before {
		t.Fatal("conflicting edit wrote")
	}
}

func TestHashlineABAChangeDoesNotReviveTarget(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\nthree\n"
	writeFixture(t, cwd, "f.txt", source)
	oldRef := seedReference(env, "f.txt", source, true)
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", oldRef, "PUT 2:\n+changed\n")); err != nil {
		t.Fatal(err)
	}
	changed := readFixture(t, cwd, "f.txt")
	newRef := seedReference(env, "f.txt", changed, true)
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", newRef, "PUT 2:\n+two\n")); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != source {
		t.Fatalf("fixture did not restore source: %q", actual)
	}
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", oldRef, "PUT 2:\n+again\n")); err == nil {
		t.Fatal("ABA restoration revived old target")
	}
}

func TestEnhancedCoverageAndValidNoop(t *testing.T) {
	for _, style := range []Style{HashlineLight, HashlineEnhanced} {
		t.Run(string(style), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			source := "one\ntwo\n"
			writeFixture(t, cwd, "f.txt", source)
			ref := seedReference(env, "f.txt", source, false)
			result, err := runPatch(t, env, style, profileInput("f.txt", ref, "PUT 2:\n+TWO\n"))
			if style == HashlineEnhanced && err == nil {
				t.Fatal("unobserved enhanced edit passed")
			}
			if style == HashlineLight && err != nil {
				t.Fatal(err)
			}
			_ = result
			noopEnv, noopCwd := editTestEnv(t)
			writeFixture(t, noopCwd, "f.txt", source)
			noopRef := seedReference(noopEnv, "f.txt", source, false)
			result, err = runPatch(t, noopEnv, style, profileInput("f.txt", noopRef, "PUT 1:\n+one\n"))
			if err != nil || string(result.Output) != "no_change\n" {
				t.Fatalf("valid no-op: %+v %v", result, err)
			}
		})
	}
}

func TestEnhancedObservedEditAndDeleteException(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\n"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 2:\n+TWO\n")); err != nil {
		t.Fatal(err)
	}
	deleteEnv, deleteCwd := editTestEnv(t)
	writeFixture(t, deleteCwd, "f.txt", source)
	deleteRef := seedReference(deleteEnv, "f.txt", source, false)
	if _, err := runPatch(t, deleteEnv, HashlineEnhanced, profileInput("f.txt", deleteRef, "REM")); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(deleteCwd, "f.txt")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("delete did not remove file: %v", err)
	}
}

func TestProfileNewFileCreationBytes(t *testing.T) {
	tests := []struct {
		name string
		body string
		want string
	}{{"blank", "+\n", ""}, {"no_final_lf", "+body", "body"}, {"trailing_blank", "+body\n+\n", "body\n"}, {"lf_and_crlf", "+first\r\n+second\r\n", "first\nsecond"}}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			env, cwd := editTestEnv(t)
			input := "[f.txt#NEW]\nPUT >$:\n" + test.body
			if _, err := runPatch(t, env, HashlineLight, input); err != nil {
				t.Fatal(err)
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != test.want {
				t.Fatalf("created %q, want %q", actual, test.want)
			}
		})
	}
}

func TestProfileQuotedPathAndBodyRows(t *testing.T) {
	env, cwd := editTestEnv(t)
	path := "odd @#].txt"
	quoted := snapshot.JSONString(path)
	input := "[" + quoted + "#NEW]\nPUT >$:\n++plus\n+-minus\n+```\n+[not-a-header]\n"
	if _, err := runPatch(t, env, HashlineEnhanced, input); err != nil {
		t.Fatal(err)
	}
	expected := "+plus\n-minus\n```\n[not-a-header]"
	if actual := readFixture(t, cwd, path); actual != expected {
		t.Fatalf("body %q, want %q", actual, expected)
	}
}

func TestProfileGrammarRejections(t *testing.T) {
	validRef := "r0123456789abcdef0123456789abcdef.1"
	inputs := []string{
		"[f.txt]\nPUT 1:\n+x\n",
		"[f.txt#A1B2]\nPUT 1:\n+x\n",
		"[f.txt@latest]\nPUT 1:\n+x\n",
		"[f.txt@" + validRef + "]\nPUT 1:\n",
		"[f.txt@" + validRef + "]\nPUT 3-2:\n+x\n",
		"*** Begin Patch\n[f.txt#NEW]\nPUT >$:\n+x\n",
	}
	for _, input := range inputs {
		if _, err := parseHashline(input, HashlineLight); err == nil {
			t.Fatalf("accepted invalid profile input %q", input)
		}
	}
}

func TestHashlineRangeAliasEquivalence(t *testing.T) {
	ref := "r0123456789abcdef0123456789abcdef.1"
	base := "[f.txt@" + ref + "]\n"
	for _, input := range []string{"PUT 2:\n+x\n", "PUT 2-2:\n+x\n", "PUT 2.=2:\n+x\n"} {
		edits, err := parseHashline(base+input, HashlineLight)
		if err != nil {
			t.Fatal(err)
		}
		if len(edits) != 1 || edits[0].locator.first != 2 || edits[0].locator.last != 2 {
			t.Fatalf("alias produced %+v", edits)
		}
	}
}

func TestRawBytesAndNoopPreservation(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := append([]byte{0xef, 0xbb, 0xbf}, []byte("a\r\nb\nc")...)
	if err := os.WriteFile(filepath.Join(cwd, "f.txt"), source, 0o644); err != nil {
		t.Fatal(err)
	}
	ref := env.Snapshots.Put("f.txt", source, []snapshot.Range{{Start: 1, End: 3}})
	if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT 2:\n+B\n")); err != nil {
		t.Fatal(err)
	}
	actual, err := os.ReadFile(filepath.Join(cwd, "f.txt"))
	if err != nil {
		t.Fatal(err)
	}
	expected := append([]byte{0xef, 0xbb, 0xbf}, []byte("a\r\nB\nc")...)
	if !bytes.Equal(actual, expected) {
		t.Fatalf("raw bytes %v, want %v", actual, expected)
	}
}

func TestUnterminatedAppendExpandsFootprint(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "a\nlast"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT >$:\n+x\n")); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "a\nlastx\n" {
		t.Fatalf("append %q", actual)
	}
	if _, err := runPatch(t, env, HashlineLight, profileInput("f.txt", ref, "PUT 2:\n+other\n")); err == nil {
		t.Fatal("unterminated append did not invalidate old final line")
	}
}

func TestDuplicateSectionsAndBatchConflicts(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\nthree\n"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	input := profileInput("f.txt", ref, "PUT 1:\n+ONE\n") + profileInput("f.txt", ref, "PUT 3:\n+THREE\n")
	if _, err := runPatch(t, env, HashlineEnhanced, input); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "ONE\ntwo\nTHREE\n" {
		t.Fatalf("merged sections %q", actual)
	}
	conflictRef := seedReference(env, "f.txt", readFixture(t, cwd, "f.txt"), true)
	conflict := profileInput("f.txt", conflictRef, "PUT <2:\n+a\nPUT <2:\n+b\n")
	if _, err := runPatch(t, env, HashlineLight, conflict); err == nil {
		t.Fatal("same-gap inserts passed")
	}
	boundary := profileInput("f.txt", conflictRef, "PUT 2:\n+TWO\nPUT <3:\n+boundary\n")
	if _, err := runPatch(t, env, HashlineLight, boundary); err == nil {
		t.Fatal("insertion at edited boundary passed")
	}
}

func TestMixedNoopsApplyOnlyEffectiveChanges(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\nthree\n"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, false)
	input := profileInput("f.txt", ref, "PUT 1:\n+one\nPUT 3:\n+THREE\n")
	result, err := runPatch(t, env, HashlineLight, input)
	if err != nil || !result.OK {
		t.Fatalf("mixed no-op: %+v %v", result, err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "one\ntwo\nTHREE\n" {
		t.Fatalf("mixed no-op result %q", actual)
	}
}

func TestBatchRefusalWritesNoValidPrefix(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.txt", "one\n")
	writeFixture(t, cwd, "b.txt", "two\n")
	refA := seedReference(env, "a.txt", "one\n", true)
	input := profileInput("a.txt", refA, "PUT 1:\n+ONE\n") + "[b.txt#A1B2]\nPUT 1:\n+TWO\n"
	if _, err := runPatch(t, env, HashlineLight, input); err == nil {
		t.Fatal("invalid second file passed")
	}
	if actual := readFixture(t, cwd, "a.txt"); actual != "one\n" {
		t.Fatalf("valid prefix wrote %q", actual)
	}
}

func TestCreateAndRenameApprovalRaces(t *testing.T) {
	env, cwd := editTestEnv(t)
	create := New("replace")
	plan, err := create.Plan(t.Context(), tool.Call{Args: jsontext.Value(`{"changes":[{"path":"new.txt","create":"new\n"}]}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	writeFixture(t, cwd, "new.txt", "competitor\n")
	if _, applyErr := plan.Apply(t.Context()); applyErr == nil {
		t.Fatal("creation clobbered racing destination")
	}
	if actual := readFixture(t, cwd, "new.txt"); actual != "competitor\n" {
		t.Fatalf("racing destination changed %q", actual)
	}
	writeFixture(t, cwd, "a.txt", "alpha\n")
	ref := seedReference(env, "a.txt", "alpha\n", true)
	renamePlan, err := New("hashline-light").Plan(t.Context(), tool.Call{Args: jsontext.Value(snapshot.JSONString(profileInput("a.txt", ref, "MV b.txt")))}, env)
	if err != nil {
		t.Fatal(err)
	}
	writeFixture(t, cwd, "b.txt", "competitor\n")
	if _, err := renamePlan.Apply(t.Context()); err == nil {
		t.Fatal("rename clobbered racing destination")
	}
	if actual := readFixture(t, cwd, "a.txt"); actual != "alpha\n" {
		t.Fatalf("rename source changed %q", actual)
	}
}

func TestMoveAfterEditInvalidatesOldPath(t *testing.T) {
	env, cwd := editTestEnv(t)
	source := "one\ntwo\n"
	writeFixture(t, cwd, "a.txt", source)
	ref := seedReference(env, "a.txt", source, true)
	input := profileInput("a.txt", ref, "PUT 1:\n+ONE\nMV b.txt")
	if _, err := runPatch(t, env, HashlineEnhanced, input); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, "b.txt"); actual != "ONE\ntwo\n" {
		t.Fatalf("rename destination %q", actual)
	}
	if _, err := os.Stat(filepath.Join(cwd, "a.txt")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("rename source remains: %v", err)
	}
	writeFixture(t, cwd, "a.txt", source)
	if _, err := runPatch(t, env, HashlineLight, profileInput("a.txt", ref, "PUT 1:\n+again\n")); err == nil {
		t.Fatal("old-path reference followed rename")
	}
}

func TestApprovalGapDriftRejectsPreparedPlan(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", "before\n")
	editor := New("replace")
	plan, err := editor.Plan(t.Context(), tool.Call{Args: jsontext.Value(`{"changes":[{"path":"f.txt","old":"before","new":"after"}]}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	writeFixture(t, cwd, "f.txt", "drift\n")
	if _, err := plan.Apply(t.Context()); err == nil {
		t.Fatal("approval-gap drift committed")
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "drift\n" {
		t.Fatalf("drift overwritten %q", actual)
	}
}

func TestCancellationBeforeFirstMutation(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "f.txt", "before\n")
	editor := New("replace")
	plan, err := editor.Plan(t.Context(), tool.Call{Args: jsontext.Value(`{"changes":[{"path":"f.txt","old":"before","new":"after"}]}`)}, env)
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := plan.Apply(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancellation error %v", err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != "before\n" {
		t.Fatalf("cancelled write changed %q", actual)
	}
}

func TestUnchangedFileMode(t *testing.T) {
	if os.Getuid() == 0 {
		t.Skip("mode check is not meaningful as root")
	}
	env, cwd := editTestEnv(t)
	path := filepath.Join(cwd, "f.txt")
	writeFixture(t, cwd, "f.txt", "before\n")
	if err := os.Chmod(path, 0o600); err != nil {
		t.Fatal(err)
	}
	if _, err := runPatch(t, env, Replace, `{"changes":[{"path":"f.txt","old":"before","new":"after"}]}`); err != nil {
		t.Fatal(err)
	}
	assertOwnerOnly(t, path)
}

func TestSpecNames(t *testing.T) {
	for _, style := range []Style{Anchor, Replace, Hashline, HashlineLight, HashlineEnhanced, ApplyPatch} {
		spec := New(string(style)).Spec()
		wantName, wantFreeform := "patch", false
		if style == ApplyPatch {
			wantName, wantFreeform = "apply_patch", true
		}
		if spec.Name != wantName || spec.Freeform != wantFreeform {
			t.Fatalf("%s spec %+v", style, spec)
		}
		if strings.Contains(spec.Description, "N*") {
			t.Fatalf("%s description advertises removed symbol grammar", style)
		}
	}
}
