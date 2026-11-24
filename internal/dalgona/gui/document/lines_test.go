package document

import (
	"strings"
	"testing"
)

func spanClusters(text string, span Span) []Cluster {
	clusters := uniform(text[:span.End], 10, false)
	for len(clusters) > 0 && clusters[0].Text.Start < span.Start {
		clusters = clusters[1:]
	}
	return breakable(clusters, afterSpaces(text[:span.End])...)
}

func uniformLayout(scope *Scope, v *Viewport, width float32) Layout {
	return func(row int) (*Flow, bool) {
		ref := v.Row(row)
		doc, ok := scope.Loaded(ref.Entry)
		if !ok || ref.Block < 0 {
			return nil, false
		}
		b := doc.Block(ref.Block)
		span := b.Rows[ref.Row]
		f := Arrange(b.Projection.Text, span, spanClusters(b.Projection.Text, span), Measure{MaxWidth: width, LineHeight: 16, Inset: 5})
		return &f, true
	}
}

type lineFixture struct {
	scope  *Scope
	v      *Viewport
	first  *Document
	second *Document
	layout Layout
}

func lineScope(t *testing.T) lineFixture {
	t.Helper()
	scope := NewScope("local", "thread-1")
	first := markdown(t, "first", "alpha beta gamma delta\n\nshort\n\n```\none\ntwo\n```\n")
	second := markdown(t, "second", "next doc")
	for _, d := range []*Document{first, second} {
		if _, err := scope.Put(d); err != nil {
			t.Fatal(err)
		}
	}
	v := NewViewport()
	v.Sync(scope, fixedRows)
	v.Resize(400)
	return lineFixture{scope: scope, v: v, first: first, second: second, layout: uniformLayout(scope, v, 120)}
}

type lineStop struct {
	doc    *Document
	offset int
}

func (x lineFixture) walk(t *testing.T, from Position, goal float32, move func(*Scope, Position, float32, Layout) (Position, bool, error), stops []lineStop) Position {
	t.Helper()
	p := from
	for i, stop := range stops {
		next, moved, err := move(x.scope, p, goal, x.layout)
		if err != nil || !moved || next.Document != stop.doc.Identity() || next.Offset != stop.offset {
			t.Fatalf("step %d reached %+v moved %v (%v), want offset %d of %v", i, next, moved, err, stop.offset, stop.doc.Identity().Item)
		}
		p = next
	}
	if _, moved, err := move(x.scope, p, goal, x.layout); moved || err != nil {
		t.Fatalf("moved past the last line from %+v (%v)", p, err)
	}
	return p
}

func TestLineMovementKeepsTheGoalAcrossVisualLinesRowsAndDocuments(t *testing.T) {
	x := lineScope(t)
	at := positionAt(t, x.first, "beta", 0, After)
	goal, err := x.v.CaretX(x.scope, at, x.layout)
	if err != nil || goal != 65 {
		t.Fatalf("goal %v (%v), want 65", goal, err)
	}
	source := x.first.Source()
	down := []lineStop{
		{x.first, strings.Index(source, "delta")},
		{x.first, strings.Index(source, "short") + len("short")},
		{x.first, strings.Index(source, "one") + len("one")},
		{x.first, strings.Index(source, "two") + len("two")},
		{x.second, strings.Index(x.second.Source(), "oc")},
	}
	last := x.walk(t, at, goal, x.v.LineDown, down)
	up := []lineStop{down[3], down[2], down[1], down[0], {x.first, at.Offset}}
	x.walk(t, last, goal, x.v.LineUp, up)
}

func TestHomeAndEndStopAtTheVisualLineEdges(t *testing.T) {
	x := lineScope(t)
	source := x.first.Source()
	cases := []struct {
		from       Position
		start, end int
	}{
		{positionAt(t, x.first, "beta", 2, After), 0, strings.Index(source, "gamma")},
		{positionAt(t, x.first, "delta", 1, After), strings.Index(source, "gamma"), strings.Index(source, "delta") + len("delta")},
		{positionAt(t, x.first, "two", 1, After), strings.Index(source, "two"), strings.Index(source, "two") + len("two")},
		{positionAt(t, x.first, "gamma", 0, Before), 0, strings.Index(source, "gamma")},
	}
	for _, c := range cases {
		start, startErr := x.v.LineStart(x.scope, c.from, x.layout)
		if startErr != nil || start.Offset != c.start || start.Affinity != After {
			t.Errorf("Home from %d %v reached %+v (%v), want %d", c.from.Offset, c.from.Affinity, start, startErr, c.start)
		}
		end, endErr := x.v.LineEnd(x.scope, c.from, x.layout)
		if endErr != nil || end.Offset != c.end || end.Affinity != Before {
			t.Errorf("End from %d %v reached %+v (%v), want %d", c.from.Offset, c.from.Affinity, end, endErr, c.end)
		}
	}
}

func TestTripleClickSelectsTheVisualLine(t *testing.T) {
	x := lineScope(t)
	cases := []struct {
		row  int
		y    float32
		want string
	}{
		{0, 3, "alpha beta "},
		{0, 20, "gamma delta"},
		{2, 8, "one\n"},
	}
	for _, c := range cases {
		f, _ := x.layout(c.row)
		sel, err := x.first.LineSelection(x.v.Row(c.row).Block, f, c.y)
		if err != nil {
			t.Fatal(err)
		}
		if text, _ := x.first.Text(sel.Anchor, sel.Focus); text != c.want {
			t.Errorf("row %d at y %v selected %q, want %q", c.row, c.y, text, c.want)
		}
	}
}
