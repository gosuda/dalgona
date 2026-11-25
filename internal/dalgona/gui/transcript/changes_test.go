package transcript

import (
	json "encoding/json/v2"
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestWireChangeDecodesKindAndCounts(t *testing.T) {
	var got wireChange
	raw := `{"path":"a.go","kind":{"type":"update"},"diff":"@@\n","diffOmitted":true,"additions":2}`
	if err := json.Unmarshal([]byte(raw), &got); err != nil {
		t.Fatalf("wire unmarshal returns %v, want nil", err)
	}
	if got.Path != "a.go" {
		t.Errorf("path reads %q, want %q", got.Path, "a.go")
	}
	if got.Kind.Type != rpc.KindUpdate {
		t.Errorf("kind reads %q, want %q", got.Kind.Type, rpc.KindUpdate)
	}
	if got.Diff != "@@\n" {
		t.Errorf("diff reads %q, want %q", got.Diff, "@@\n")
	}
	if !got.DiffOmitted {
		t.Error("omitted reads false, want true")
	}
	if got.Additions == nil || *got.Additions != 2 {
		t.Errorf("additions reads %+v, want 2", got.Additions)
	}
	if got.Deletions != nil {
		t.Errorf("deletions reads %+v, want nil", got.Deletions)
	}
}

type parseChangeWant struct {
	path     string
	kind     string
	diff     string
	omitted  bool
	adds     int
	counted  bool
	dels     int
	delCount bool
}

func checkParseIdentity(t *testing.T, name string, c change, want parseChangeWant) {
	t.Helper()
	if c.Path != want.path {
		t.Errorf("%s: path reads %q, want %q", name, c.Path, want.path)
	}
	if c.Kind != want.kind {
		t.Errorf("%s: kind reads %q, want %q", name, c.Kind, want.kind)
	}
	if c.Diff != want.diff {
		t.Errorf("%s: diff reads %q, want %q", name, c.Diff, want.diff)
	}
	if c.Omitted != want.omitted {
		t.Errorf("%s: omitted reads %v, want %v", name, c.Omitted, want.omitted)
	}
}

func checkParseCounts(t *testing.T, name string, c change, want parseChangeWant) {
	t.Helper()
	if c.Additions != want.adds || c.Counted != want.counted {
		t.Errorf("%s: additions reads %d counted %v, want %d %v", name, c.Additions, c.Counted, want.adds, want.counted)
	}
	if c.Deletions != want.dels || c.DeleteCount != want.delCount {
		t.Errorf("%s: deletions reads %d deleteCount %v, want %d %v", name, c.Deletions, c.DeleteCount, want.dels, want.delCount)
	}
}

func checkParseChange(t *testing.T, name, raw string, want parseChangeWant) {
	t.Helper()
	got, err := parseChanges([]byte(raw))
	if err != nil {
		t.Fatalf("%s: parse returns %v, want nil", name, err)
	}
	if len(got) != 1 {
		t.Fatalf("%s: parse holds %d changes, want 1", name, len(got))
	}
	c := got[0]
	checkParseIdentity(t, name, c, want)
	checkParseCounts(t, name, c, want)
}

func TestParseChangesMapsWireFields(t *testing.T) {
	cases := []struct {
		name string
		raw  string
		want parseChangeWant
	}{
		{"full", `[{"path":"a.go","kind":{"type":"update"},"diff":"@@ -1 +1 @@\n-old\n+new\n","diffOmitted":false,"additions":1,"deletions":2}]`, parseChangeWant{path: "a.go", kind: rpc.KindUpdate, diff: "@@ -1 +1 @@\n-old\n+new\n", omitted: false, adds: 1, counted: true, dels: 2, delCount: true}},
		{"omitted", `[{"path":"skip.go","kind":{"type":"update"},"diffOmitted":true}]`, parseChangeWant{path: "skip.go", kind: rpc.KindUpdate, diff: "", omitted: true, adds: 0, counted: false, dels: 0, delCount: false}},
		{"adds only", `[{"path":"n.go","kind":{"type":"add"},"diff":"fresh","additions":3}]`, parseChangeWant{path: "n.go", kind: rpc.KindAdd, diff: "fresh", omitted: false, adds: 3, counted: true, dels: 0, delCount: false}},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkParseChange(t, c.name, c.raw, c.want) })
	}
}

func TestParseChangesRejectsInvalidJSON(t *testing.T) {
	_, err := parseChanges([]byte(`not json`))
	if err == nil {
		t.Fatal("parse of invalid JSON returns nil, want an error")
	}
	if errors.Is(err, errPatchOmitted) {
		t.Fatalf("parse of invalid JSON returns %v, want a syntax error", err)
	}
}

func TestLiveChangesMirrorsOmission(t *testing.T) {
	cases := []liveChangeCheck{
		{name: "counted", in: rpc.FileChange{Path: "a.go", Kind: rpc.KindAdd, Diff: "d", Additions: 5, Deletions: 2}, adds: 5, dels: 2, count: true, omit: false},
		{name: "omitted", in: rpc.FileChange{Path: "b.go", Kind: rpc.KindUpdate, Additions: 9, Deletions: 7, DiffOmitted: true}, adds: 9, dels: 7, count: false, omit: true},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) { checkLiveChange(t, c) })
	}
	if got := liveChanges(nil); len(got) != 0 {
		t.Fatalf("live of no changes holds %d, want 0", len(got))
	}
}

type liveChangeCheck struct {
	name  string
	in    rpc.FileChange
	adds  int
	dels  int
	count bool
	omit  bool
}

func checkLiveIdentity(t *testing.T, c liveChangeCheck, out change) {
	t.Helper()
	if out.Path != c.in.Path || out.Kind != c.in.Kind || out.Diff != c.in.Diff || out.Omitted != c.omit {
		t.Errorf("%s: identity reads %+v, want path %q kind %q omitted %v", c.name, out, c.in.Path, c.in.Kind, c.omit)
	}
}

func checkLiveCounts(t *testing.T, c liveChangeCheck, out change) {
	t.Helper()
	if out.Additions != c.adds || out.Counted != c.count {
		t.Errorf("%s: additions reads %d counted %v, want %d %v", c.name, out.Additions, out.Counted, c.adds, c.count)
	}
	if out.Deletions != c.dels || out.DeleteCount != c.count {
		t.Errorf("%s: deletions reads %d deleteCount %v, want %d %v", c.name, out.Deletions, out.DeleteCount, c.dels, c.count)
	}
}

func checkLiveChange(t *testing.T, c liveChangeCheck) {
	t.Helper()
	got := liveChanges([]rpc.FileChange{c.in})
	if len(got) != 1 {
		t.Fatalf("%s: live holds %d changes, want 1", c.name, len(got))
	}
	out := got[0]
	checkLiveIdentity(t, c, out)
	checkLiveCounts(t, c, out)
}
