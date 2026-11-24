package code

import (
	"strings"
	"testing"
)

func TestComputeApplyRoundTrip(t *testing.T) {
	cases := map[string][2]string{
		"modify":        {"a\nb\nc\nd\ne\nf\ng\n", "a\nB\nc\nd\nE\nf\ng\n"},
		"add file":      {"", "line one\nline two\n"},
		"delete file":   {"line one\nline two\n", ""},
		"equal":         {"same\n", "same\n"},
		"no trailing":   {"a\nb", "a\nB"},
		"add trailing":  {"a\n", "a\nb\n"},
		"multibyte":     {"héllo\n世界\n", "héllo!\n世界 🌍\n"},
		"insert middle": {"1\n2\n3\n4\n5\n6\n7\n8\n9\n", "1\n2\nX\nY\n3\n4\n5\n6\n7\n8\n9\n"},
	}
	for name, c := range cases {
		f, err := Compute("a/f", "b/f", c[0], c[1])
		if err != nil {
			t.Fatalf("%s: Compute error: %v", name, err)
		}
		got, err := f.Apply(c[0])
		if err != nil {
			t.Fatalf("%s: Apply error: %v", name, err)
		}
		if got != c[1] {
			t.Fatalf("%s: Apply result %q, want %q", name, got, c[1])
		}
	}
}

func TestPatchRoundTrip(t *testing.T) {
	before := "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n"
	after := "1\n2\nchanged\n4\n5\n6\n7\n8\n9\n10\n11\ntwelve\n"
	f, err := Compute("a/f", "b/f", before, after)
	if err != nil {
		t.Fatalf("Compute error: %v", err)
	}
	text := f.Patch()
	if text == "" {
		t.Fatal("nonempty diff rendered an empty patch")
	}
	back, err := ParseUnified(text)
	if err != nil {
		t.Fatalf("ParseUnified error: %v", err)
	}
	if len(back.Hunks) != len(f.Hunks) {
		t.Fatalf("round trip changed hunk count: %d vs %d", len(back.Hunks), len(f.Hunks))
	}
	got, err := back.Apply(before)
	if err != nil {
		t.Fatalf("reparsed Apply error: %v", err)
	}
	if got != after {
		t.Fatalf("reparsed Apply result %q, want %q", got, after)
	}
}

func TestCountsMatchChanges(t *testing.T) {
	before := "keep\nold\nkeep\n"
	after := "keep\nnew\nkeep\nextra\n"
	f, err := Compute("a/f", "b/f", before, after)
	if err != nil {
		t.Fatalf("Compute error: %v", err)
	}
	if f.Additions() != 2 || f.Deletions() != 1 {
		t.Fatalf("counts = +%d/-%d, want +2/-1", f.Additions(), f.Deletions())
	}
}

func TestCollapseKeepsMapping(t *testing.T) {
	before := "l01\nl02\nl03\nl04\nl05\nl06\nl07\nl08\nl09\nl10\nl11\nl12\n"
	after := "l01\nL02\nl03\nl04\nl05\nl06\nl07\nl08\nl09\nl10\nl11\nL12\n"
	f, err := Compute("a/f", "b/f", before, after)
	if err != nil {
		t.Fatalf("Compute error: %v", err)
	}
	full := mapOldAll(t, f, before)
	rows := f.Rows(1)
	if countHidden(rows) == 0 {
		t.Fatal("collapse hid no context lines")
	}
	checkSurvivorsMapped(t, f, full)
	checkVisibleRows(t, f, rows)
}

func countHidden(rows []Row) int {
	hidden := 0
	for _, r := range rows {
		if r.IsGap {
			hidden += r.GapOldCount
			continue
		}
		if r.Line.Op == ContextOp {
			continue
		}
	}
	return hidden
}

func checkSurvivorsMapped(t *testing.T, f File, full map[int]int) {
	t.Helper()
	for old, want := range full {
		got, ok := f.MapOld(old)
		if !ok {
			t.Fatalf("MapOld(%d) lost after collapse setup", old)
		}
		if got != want {
			t.Fatalf("MapOld(%d) = %d, want %d", old, got, want)
		}
	}
}

func checkVisibleRows(t *testing.T, f File, rows []Row) {
	t.Helper()
	for _, r := range rows {
		if r.IsGap {
			continue
		}
		checkVisibleRow(t, f, r.Line)
	}
}

func checkVisibleRow(t *testing.T, f File, l Line) {
	t.Helper()
	if l.Op == ContextOp || l.Op == AddOp {
		if _, ok := f.MapOld(l.Old); l.Op == ContextOp && !ok {
			t.Fatalf("visible context old line %d unmapped", l.Old)
		}
	}
	if l.Op == DeleteOp {
		if _, ok := f.MapOld(l.Old); ok {
			t.Fatalf("deleted old line %d must not map", l.Old)
		}
	}
}

func mapOldAll(t *testing.T, f File, before string) map[int]int {
	t.Helper()
	deleted := map[int]bool{}
	for _, h := range f.Hunks {
		for _, l := range h.Lines {
			if l.Op == DeleteOp {
				deleted[l.Old] = true
			}
		}
	}
	n := strings.Count(before, "\n")
	out := map[int]int{}
	for old := 1; old <= n; old++ {
		fresh, ok := f.MapOld(old)
		if deleted[old] {
			if ok {
				t.Fatalf("MapOld(%d) mapped a deleted line", old)
			}
			continue
		}
		if !ok {
			t.Fatalf("MapOld(%d) lost a surviving line", old)
		}
		out[old] = fresh
	}
	return out
}

func TestDiffLimitsOmitNeverEmpty(t *testing.T) {
	before := strings.Repeat("x\n", MaxDiffBytes/2+8)
	after := before + "tail\n"
	f, err := Compute("a/big", "b/big", before, after)
	if err == nil {
		t.Fatal("oversized diff must report the diff budget")
	}
	if f.Kind != Omitted {
		t.Fatalf("oversized diff kind = %v, want Omitted", f.Kind)
	}
	if f.Patch() == "" {
		t.Fatal("omitted diff must never appear as an empty change")
	}
	if f.OldName != "a/big" || f.NewName != "b/big" {
		t.Fatal("omitted diff must preserve file names")
	}
}

func TestParseStates(t *testing.T) {
	checkAddParse(t)
	checkDeleteParse(t)
	checkBinaryParse(t)
	checkRenameParse(t)
	checkOmittedFile(t)
}

func checkAddParse(t *testing.T) {
	t.Helper()
	add, err := ParseUnified("--- /dev/null\n+++ b/new\n@@ -0,0 +1 @@\n+hi\n")
	if err != nil {
		t.Fatalf("add parse error: %v", err)
	}
	if add.Kind != Add {
		t.Fatalf("add kind = %v", add.Kind)
	}
}

func checkDeleteParse(t *testing.T) {
	t.Helper()
	del, err := ParseUnified("--- a/old\n+++ /dev/null\n@@ -1 +0,0 @@\n-hi\n")
	if err != nil {
		t.Fatalf("delete parse error: %v", err)
	}
	if del.Kind != Delete {
		t.Fatalf("delete kind = %v", del.Kind)
	}
}

func checkBinaryParse(t *testing.T) {
	t.Helper()
	bin, err := ParseUnified("Binary files a/img.png and b/img.png differ\n")
	if err != nil {
		t.Fatalf("binary parse error: %v", err)
	}
	if bin.Kind != Binary {
		t.Fatalf("binary kind = %v", bin.Kind)
	}
}

func checkRenameParse(t *testing.T) {
	t.Helper()
	ren, err := ParseUnified("rename from a/old\nrename to b/new\n")
	if err != nil {
		t.Fatalf("rename parse error: %v", err)
	}
	if ren.Kind != Rename {
		t.Fatalf("rename kind = %v", ren.Kind)
	}
}

func checkOmittedFile(t *testing.T) {
	t.Helper()
	omit := OmittedFile("a/x", "b/x", "either image is over 1 MiB")
	if omit.Kind != Omitted || omit.Patch() == "" {
		t.Fatal("omitted file must carry its names and a visible explanation")
	}
}

func TestParseErrors(t *testing.T) {
	for _, text := range []string{
		"--- a/x\n+++ b/x\n@@ -1 +1 @@\n?",
		"--- a/x\n+++ b/x\n@@ -x +1 @@\n a\n",
		"--- a/x\n+++ b/x\n@@ -1,2 +1 @@\n a\n",
		"@@ -1 +1 @@\n a\n",
	} {
		if _, err := ParseUnified(text); err == nil {
			t.Fatalf("malformed diff %q parsed without error", text)
		}
	}
	if _, err := ParseUnified("--- a/x\n+++ b/x\n@@ -1 +1 @@\n a\xff\n"); err == nil {
		t.Fatal("invalid UTF-8 diff must be refused")
	}
}
