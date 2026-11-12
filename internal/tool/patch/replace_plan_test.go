package patch

import (
	"bytes"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func TestPlanReplaceRefusesChangedSource(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.txt", "two\n")
	_, err := PlanReplace(t.Context(), env, "", []ReplacedFile{{
		Path: "a.txt", Before: []byte("one\n"), After: []byte("uno\n"),
	}})
	const want = "patch: a.txt changed while the rewrite was planned. Nothing was written. Try again"
	if err == nil || err.Error() != want {
		t.Fatalf("PlanReplace error = %v, want %q", err, want)
	}
	if got := readFixture(t, cwd, "a.txt"); got != "two\n" {
		t.Fatalf("file changed to %q after a refused plan", got)
	}
}

func TestPlanReplaceApplyWritesAndSummarises(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.txt", "one\ntwo\n")
	const summary = "rewrite: 1 replacements in 1 files\na.txt: 1 replacements\n"
	plan, err := PlanReplace(t.Context(), env, summary, []ReplacedFile{{
		Path:    "a.txt",
		Before:  []byte("one\ntwo\n"),
		After:   []byte("uno\ntwo\n"),
		Splices: []snapshot.Splice{{Start: 0, End: 3, Inserted: 3}},
	}})
	if err != nil {
		t.Fatal(err)
	}
	result, err := plan.Apply(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	want := summary + "[a.txt#" + snapshot.Tag("whole", []byte("uno\ntwo\n")) + "]\n1:uno\n2:two\n"
	if !strings.HasPrefix(string(result.Output), want) {
		t.Fatalf("output = %q, want prefix %q", result.Output, want)
	}
	if got := readFixture(t, cwd, "a.txt"); got != "uno\ntwo\n" {
		t.Fatalf("file = %q", got)
	}
}

func TestPlanReplaceKeepsPreImage(t *testing.T) {
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "a.txt", "one\ntwo\n")
	plan, err := PlanReplace(t.Context(), env, "", []ReplacedFile{{
		Path:    "a.txt",
		Before:  []byte("one\ntwo\n"),
		After:   []byte("uno\ntwo\n"),
		Splices: []snapshot.Splice{{Start: 0, End: 3, Inserted: 3}},
	}})
	if err != nil {
		t.Fatal(err)
	}
	defer plan.Discard()
	if len(plan.Files) != 1 {
		t.Fatalf("plan files = %+v", plan.Files)
	}
	file := plan.Files[0]
	if !bytes.Equal(file.Pre, []byte("one\ntwo\n")) {
		t.Fatalf("pre-image = %q", file.Pre)
	}
	if file.PreSHA != snapshot.Digest([]byte("one\ntwo\n")) {
		t.Fatalf("pre digest = %q", file.PreSHA)
	}
}
