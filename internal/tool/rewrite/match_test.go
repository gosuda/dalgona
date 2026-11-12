package rewrite

import (
	"reflect"
	"testing"

	"github.com/gosuda/dalgona/internal/syntax"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func mustTemplate(t *testing.T, text string) template {
	t.Helper()
	tmpl, err := parseTemplate(text)
	if err != nil {
		t.Fatal(err)
	}
	return tmpl
}

func TestEditsOfReplacesTargetOnly(t *testing.T) {
	src := []byte("func A() {}\nfunc B() {}\n")
	groups := []group{
		{line: 1, spans: map[string]span{"name": {start: 5, end: 6}}},
		{line: 2, spans: map[string]span{"name": {start: 17, end: 18}}},
	}
	edits, err := editsOf("a.go", src, groups, "name", mustTemplate(t, "X"))
	if err != nil {
		t.Fatal(err)
	}
	if got := string(apply(src, edits)); got != "func X() {}\nfunc X() {}\n" {
		t.Fatalf("apply = %q", got)
	}
}

func TestEditsOfRefusesAbsentTarget(t *testing.T) {
	groups := []group{{line: 3, spans: map[string]span{"other": {start: 0, end: 1}}}}
	_, err := editsOf("a.go", []byte("x"), groups, "name", mustTemplate(t, "X"))
	if err == nil || err.Error() != "rewrite: target name is not in the match at line 3" {
		t.Fatalf("editsOf error = %v", err)
	}
}

func TestEditsOfRefusesZeroWidthTarget(t *testing.T) {
	groups := []group{{line: 3, spans: map[string]span{"name": {start: 5, end: 5}}}}
	_, err := editsOf("a.go", []byte("0123456789"), groups, "name", mustTemplate(t, "X"))
	if err == nil || err.Error() != "rewrite: target name has zero width at line 3" {
		t.Fatalf("editsOf error = %v", err)
	}
}

func TestEditsOfRefusesOverlap(t *testing.T) {
	groups := []group{
		{line: 1, spans: map[string]span{"name": {start: 0, end: 10}}},
		{line: 1, spans: map[string]span{"name": {start: 5, end: 15}}},
	}
	_, err := editsOf("a.go", make([]byte, 20), groups, "name", mustTemplate(t, "X"))
	if err == nil || err.Error() != "rewrite: targets overlap in a.go at line 1" {
		t.Fatalf("editsOf error = %v", err)
	}
}

func TestEditsOfRefusesAbsentCapture(t *testing.T) {
	groups := []group{{line: 3, spans: map[string]span{"name": {start: 0, end: 1}}}}
	_, err := editsOf("a.go", []byte("x"), groups, "name", mustTemplate(t, "@other"))
	if err == nil || err.Error() != "rewrite: @other is not in the match at line 3" {
		t.Fatalf("editsOf error = %v", err)
	}
}

func TestGroupMatchesRefusesRepeatedName(t *testing.T) {
	captures := []syntax.Capture{
		{Name: "name", Line: 1, StartByte: 0, EndByte: 1, Match: 0},
		{Name: "name", Line: 1, StartByte: 2, EndByte: 3, Match: 0},
	}
	_, err := groupMatches(captures)
	if err == nil || err.Error() != "rewrite: @name occurs twice in one match at line 1" {
		t.Fatalf("groupMatches error = %v", err)
	}
}

func TestGroupMatchesSplitsByMatch(t *testing.T) {
	captures := []syntax.Capture{
		{Name: "name", Line: 1, StartByte: 0, EndByte: 1, Match: 0},
		{Name: "name", Line: 2, StartByte: 5, EndByte: 6, Match: 1},
	}
	groups, err := groupMatches(captures)
	if err != nil {
		t.Fatal(err)
	}
	if len(groups) != 2 || groups[0].line != 1 || groups[1].line != 2 {
		t.Fatalf("groupMatches lines = %#v, want two groups at lines 1 and 2", groups)
	}
	if groups[1].spans["name"] != (span{start: 5, end: 6}) {
		t.Fatalf("second group span = %v", groups[1].spans["name"])
	}
}

func TestApplyAndSplicesOf(t *testing.T) {
	edits := []edit{
		{start: 0, end: 1, text: []byte("X")},
		{start: 2, end: 3, text: []byte("YY")},
	}
	if got := string(apply([]byte("abc"), edits)); got != "XbYY" {
		t.Fatalf("apply = %q, want %q", got, "XbYY")
	}
	want := []snapshot.Splice{{Start: 0, End: 1, Inserted: 1}, {Start: 2, End: 3, Inserted: 2}}
	if got := splicesOf(edits); !reflect.DeepEqual(got, want) {
		t.Fatalf("splicesOf = %v, want %v", got, want)
	}
}
