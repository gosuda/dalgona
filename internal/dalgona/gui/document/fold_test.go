package document

import (
	"fmt"
	"math"
	"strings"
	"testing"
)

func codeLines(count int) string {
	var b strings.Builder
	b.WriteString("```go\n")
	for i := range count {
		fmt.Fprintf(&b, "line %d\n", i)
	}
	b.WriteString("```\n")
	return b.String()
}

type expectedFold struct {
	kind   FoldKind
	blocks Span
	rows   int
	start  string
}

func checkFold(t *testing.T, d *Document, f Fold, want expectedFold) {
	t.Helper()
	first, last := d.Block(want.blocks.Start), d.Block(want.blocks.End-1)
	if f.Kind != want.kind || f.Blocks != want.blocks || f.Rows != want.rows || f.Key != first.Key {
		t.Fatalf("fold %+v, want %+v", f, want)
	}
	if f.Start.Offset != strings.Index(d.Source(), want.start) || f.Source != (Span{Start: first.Span.Start, End: last.Span.End}) {
		t.Fatalf("fold starts at %d and covers %v", f.Start.Offset, f.Source)
	}
}

func TestFoldsCoverLongCodeAndQuotes(t *testing.T) {
	source := "intro\n\n" + codeLines(LongCodeRows) + "\n" + codeLines(LongCodeRows-1) + "\n> quoted one\n>\n> quoted two\n\n> single\n\nafter\n"
	d := markdown(t, "folds", source)
	want := []expectedFold{
		{FoldCode, Span{Start: 1, End: 2}, LongCodeRows, "line 0"},
		{FoldQuote, Span{Start: 3, End: 5}, 2, "quoted one"},
	}
	folds := d.Folds()
	if len(folds) != len(want) {
		t.Fatalf("folds %+v, want %+v", folds, want)
	}
	for i := range want {
		checkFold(t, d, folds[i], want[i])
	}
}

func TestEveryMultilineDiffSectionFolds(t *testing.T) {
	patch := patchDocument(t, "patch", strings.Join(patchSections, ""), false)
	folds := patch.Folds()
	if len(folds) != len(patchSections)-1 {
		t.Fatalf("%d foldable diff sections, want every section with more than one line", len(folds))
	}
	for _, f := range folds {
		if f.Kind != FoldDiff || f.Rows != len(patch.Block(f.Blocks.Start).Rows) {
			t.Fatalf("patch fold %+v", f)
		}
	}
}

type foldFixture struct {
	st   *State
	v    *Viewport
	doc  *Document
	fold Fold
}

func foldedTranscript(t *testing.T) foldFixture {
	t.Helper()
	scope := NewScope("local", "thread-1")
	d := markdown(t, "folds", "intro\n\n"+codeLines(12)+"\nafter one\n\nafter two\n\nafter three\n\nafter four\n\nafter five\n\nafter six\n")
	if _, err := scope.Put(d); err != nil {
		t.Fatal(err)
	}
	v := NewViewport()
	v.Sync(scope, fixedRows)
	v.Resize(60)
	v.JumpToLatest()
	return foldFixture{st: NewState(scope), v: v, doc: d, fold: d.Folds()[0]}
}

func (x foldFixture) toggle() {
	x.st.ToggleFold(x.v, x.fold)
	x.v.Sync(x.st.Scope, fixedRows)
	x.v.Restore(x.st.Scope, x.st.Anchor, atRowTop)
}

func (x foldFixture) anchorTop(t *testing.T) Anchor {
	row := x.v.RowAt(x.v.Offset())
	x.st.Anchor, x.st.Anchored = x.v.Capture(row, rowStart(t, x.st.Scope, x.v.Row(row)), 0), true
	return x.st.Anchor
}

func (x foldFixture) displacement(t *testing.T, p Position) float64 {
	t.Helper()
	row, ok := x.v.Locate(x.st.Scope, p)
	if !ok {
		t.Fatalf("%+v is not in the viewport", p)
	}
	return x.v.RowTop(row) - x.v.Offset()
}

func (x foldFixture) assertRows(t *testing.T, folded bool, rows int) {
	t.Helper()
	if x.st.Scope.Folded(x.fold.Key) != folded || x.v.Rows() != rows || x.v.Row(1).Hidden != 19-rows {
		t.Fatalf("viewport has %d rows, header hides %d, folded %v; want %d rows", x.v.Rows(), x.v.Row(1).Hidden, x.st.Scope.Folded(x.fold.Key), rows)
	}
}

func TestFoldingKeepsTheReaderAnchorBelowTheFold(t *testing.T) {
	x := foldedTranscript(t)
	x.v.ScrollBy(-60)
	anchor := x.anchorTop(t)
	if !strings.HasPrefix(x.doc.Source()[anchor.Position.Offset:], "after one") {
		t.Fatalf("anchor at %q", x.doc.Source()[anchor.Position.Offset:])
	}
	x.toggle()
	x.assertRows(t, true, 19-11)
	if got := x.displacement(t, anchor.Position); math.Abs(got-float64(anchor.Displacement)) > 1 || x.st.Anchor != anchor {
		t.Fatalf("folding above moved the anchor to %v, want %v", got, anchor.Displacement)
	}
	x.toggle()
	x.assertRows(t, false, 19)
	if x.v.Offset() != 260 {
		t.Fatalf("unfolding left the viewport at offset %v", x.v.Offset())
	}
}

func TestFoldingAroundTheAnchorPinsTheFoldHeader(t *testing.T) {
	x := foldedTranscript(t)
	x.v.ScrollBy(-(x.v.Offset() - 125))
	x.anchorTop(t)
	x.toggle()
	if x.st.Anchor.Position != x.fold.Start || x.st.Anchor.Displacement != 0 {
		t.Fatalf("anchor inside the fold became %+v, want the fold start at the viewport top", x.st.Anchor)
	}
	if got := x.displacement(t, x.fold.Start); got != 0 {
		t.Fatalf("fold header sits %v below the viewport top", got)
	}
	x.toggle()
	if got := x.displacement(t, x.fold.Start); got != 0 || x.v.Offset() != 20 {
		t.Fatalf("unfolding moved the header to %v at offset %v", got, x.v.Offset())
	}
}

func TestLineMovementSkipsFoldedRows(t *testing.T) {
	x := foldedTranscript(t)
	x.toggle()
	layout := uniformLayout(x.st.Scope, x.v, 500)
	source := x.doc.Source()
	p, _ := x.doc.Start()
	for _, want := range []string{"line 0", "after one"} {
		next, moved, err := x.v.LineDown(x.st.Scope, p, 5, layout)
		if err != nil || !moved || !strings.HasPrefix(source[next.Offset:], want) {
			t.Fatalf("line down reached %q (%v), want %q", source[next.Offset:], err, want)
		}
		p = next
	}
	hidden := positionAt(t, x.doc, "line 7", 0, After)
	if row, _ := x.v.Locate(x.st.Scope, hidden); row != 1 {
		t.Fatalf("a folded position located row %d, want the fold header", row)
	}
	if next, _, err := x.v.LineDown(x.st.Scope, hidden, 5, layout); err != nil || !strings.HasPrefix(source[next.Offset:], "after one") {
		t.Fatalf("line down from folded text reached %q (%v)", source[next.Offset:], err)
	}
}

func TestFoldFollowsItsBlockWhenTheStreamCloses(t *testing.T) {
	scope := NewScope("local", "thread-1")
	open := openMarkdown(t, "stream", "intro\n\n"+strings.TrimSuffix(codeLines(12), "```\n"))
	if _, err := scope.Put(open); err != nil {
		t.Fatal(err)
	}
	st := NewState(scope)
	v := NewViewport()
	v.Sync(scope, fixedRows)
	fold := open.Folds()[0]
	if !fold.Key.Provisional {
		t.Fatalf("growing code fold key %+v", fold.Key)
	}
	st.ToggleFold(v, fold)
	closed, change, err := open.Close()
	if err != nil {
		t.Fatal(err)
	}
	if err := st.Publish(change, closed); err != nil {
		t.Fatal(err)
	}
	final := closed.Folds()[0]
	if final.Key == fold.Key || !scope.Folded(final.Key) || scope.Folded(fold.Key) {
		t.Fatalf("fold stayed on %+v after the block closed as %+v", fold.Key, final.Key)
	}
}
