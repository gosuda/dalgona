package patch

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

func TestTailGapUnderReferenceMapsToTheSnapshotEnd(t *testing.T) {
	tests := []struct {
		name        string
		cooperating string
		want        string
	}{
		{"CUT 1", "CUT 1\n", "two\nthree\nfour\n"},
		{"PUT <1", "PUT <1:\n+zero\n", "zero\none\ntwo\nthree\nfour\n"},
	}
	for _, style := range []Style{HashlineLight, HashlineEnhanced} {
		for _, test := range tests {
			t.Run("GAP-03 GAP-04/"+string(style)+"/"+test.name, func(t *testing.T) {
				requireTailAppend(t, style, test.cooperating, test.want)
			})
		}
	}
}

func requireTailAppend(t *testing.T, style Style, cooperating, want string) {
	t.Helper()
	env, cwd := editTestEnv(t)
	source := "one\ntwo\nthree\n"
	writeFixture(t, cwd, "f.txt", source)
	ref := seedReference(env, "f.txt", source, true)
	if _, err := runPatch(t, env, style, profileInput("f.txt", ref, cooperating)); err != nil {
		t.Fatal(err)
	}
	if _, err := runPatch(t, env, style, profileInput("f.txt", ref, "PUT >$:\n+four\n")); err != nil {
		t.Fatal(err)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != want {
		t.Fatalf("tail append %q, want %q", actual, want)
	}
}

func TestSecondTailInsertConflictsAfterAnAppend(t *testing.T) {
	for _, style := range []Style{HashlineLight, HashlineEnhanced} {
		t.Run("GAP-01 E-09/"+string(style), func(t *testing.T) {
			env, cwd := editTestEnv(t)
			source := "one\ntwo\nthree\n"
			writeFixture(t, cwd, "f.txt", source)
			ref := seedReference(env, "f.txt", source, true)
			if _, err := runPatch(t, env, style, profileInput("f.txt", ref, "PUT >$:\n+four\n")); err != nil {
				t.Fatal(err)
			}
			_, err := runPatch(t, env, style, profileInput("f.txt", ref, "PUT >$:\n+five\n"))
			if err == nil || !strings.HasSuffix(err.Error(), "is stale: snapshot: gap changed. Read again") {
				t.Fatalf("second tail insert error %v", err)
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != "one\ntwo\nthree\nfour\n" {
				t.Fatalf("conflicting insert wrote %q", actual)
			}
		})
	}
}

func TestFinalRowCoverageUsesTheSnapshotRow(t *testing.T) {
	tests := []struct {
		name        string
		cooperating string
		tail        string
	}{
		{"control PUT >3", "", "PUT >3:\n+four\n"},
		{"control PUT >$", "", "PUT >$:\n+four\n"},
		{"CUT 1 then PUT >3", "CUT 1\n", "PUT >3:\n+four\n"},
		{"CUT 1 then PUT >$", "CUT 1\n", "PUT >$:\n+four\n"},
	}
	for _, test := range tests {
		t.Run("E-13/"+test.name, func(t *testing.T) {
			requireFinalRowRefusal(t, test.cooperating, test.tail)
		})
	}
}

func requireFinalRowRefusal(t *testing.T, cooperating, tail string) {
	t.Helper()
	const refusal = "lines 3-3 of f.txt need prior observation. Read them, then resend"
	env, cwd := editTestEnv(t)
	source := "one\ntwo\nthree"
	writeFixture(t, cwd, "f.txt", source)
	ref := env.Snapshots.Put("f.txt", []byte(source), []snapshot.Range{{Start: 1, End: 2}})
	want := source
	if cooperating != "" {
		if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, cooperating)); err != nil {
			t.Fatal(err)
		}
		want = "two\nthree"
	}
	_, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, tail))
	if err == nil || !strings.Contains(err.Error(), refusal) {
		t.Fatalf("final row edit error %v, want %q", err, refusal)
	}
	if actual := readFixture(t, cwd, "f.txt"); actual != want {
		t.Fatalf("refused edit left %q, want %q", actual, want)
	}
}

func TestFinalRowCoverageAfterPrefixInsertion(t *testing.T) {
	for _, tail := range []string{"PUT >3:\n+four\n", "PUT >$:\n+four\n"} {
		t.Run("E-06 E-13/"+strings.SplitN(tail, ":", 2)[0], func(t *testing.T) {
			env, cwd := editTestEnv(t)
			source := "one\ntwo\nthree"
			writeFixture(t, cwd, "f.txt", source)
			ref := env.Snapshots.Put("f.txt", []byte(source), []snapshot.Range{{Start: 1, End: 3}})
			if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, "PUT <1:\n+zero\n")); err != nil {
				t.Fatal(err)
			}
			if _, err := runPatch(t, env, HashlineEnhanced, profileInput("f.txt", ref, tail)); err != nil {
				t.Fatal(err)
			}
			if actual := readFixture(t, cwd, "f.txt"); actual != "zero\none\ntwo\nthreefour\n" {
				t.Fatalf("append after prefix insertion %q", actual)
			}
		})
	}
}
