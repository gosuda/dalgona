package patch

import (
	"fmt"
	"testing"

	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

type oracleAtom struct {
	identity int
	text     string
}

type oracleCounts struct {
	rangeCases    int
	rangeAccepted int
	gapCases      int
	gapAccepted   int
}

func (c *oracleCounts) addRange(accepted bool) {
	c.rangeCases++
	if accepted {
		c.rangeAccepted++
	}
}

func (c *oracleCounts) addGap(accepted bool) {
	c.gapCases++
	if accepted {
		c.gapAccepted++
	}
}

func oracleSpans(length int) []snapshot.Span {
	var out []snapshot.Span
	for start := range length {
		for end := start + 1; end <= length; end++ {
			out = append(out, snapshot.Span{Start: start, End: end})
		}
	}
	return out
}

func oracleSplices(length, maxInserted int) []snapshot.Splice {
	var out []snapshot.Splice
	for start := 0; start <= length; start++ {
		for end := start; end <= length; end++ {
			out = append(out, oracleInsertions(start, end, maxInserted)...)
		}
	}
	return out
}

func oracleInsertions(start, end, maxInserted int) []snapshot.Splice {
	out := make([]snapshot.Splice, 0, maxInserted+1)
	for inserted := 0; inserted <= maxInserted; inserted++ {
		if start != end || inserted > 0 {
			out = append(out, snapshot.Splice{Start: start, End: end, Inserted: inserted})
		}
	}
	return out
}

func oracleFile(length int) []oracleAtom {
	out := make([]oracleAtom, length)
	for i := range out {
		out[i] = oracleAtom{identity: i, text: fmt.Sprintf("line-%d", i%2)}
	}
	return out
}

func oracleApply(source []oracleAtom, edit snapshot.Splice, fresh int) []oracleAtom {
	added := make([]oracleAtom, edit.Inserted)
	for i := range added {
		added[i] = oracleAtom{identity: fresh + i, text: fmt.Sprintf("new-%d", i%2)}
	}
	out := make([]oracleAtom, 0, len(source)-(edit.End-edit.Start)+len(added))
	out = append(out, source[:edit.Start]...)
	out = append(out, added...)
	out = append(out, source[edit.End:]...)
	return out
}

func positions(current []oracleAtom) map[int]int {
	out := make(map[int]int, len(current))
	for i, atom := range current {
		out[atom.identity] = i
	}
	return out
}

func spanOracle(original, current []oracleAtom, target snapshot.Span) (snapshot.Span, bool) {
	at := positions(current)
	first, ok := at[original[target.Start].identity]
	if !ok {
		return snapshot.Span{}, false
	}
	for i, atom := range original[target.Start:target.End] {
		if position, found := at[atom.identity]; !found || position != first+i {
			return snapshot.Span{}, false
		}
	}
	return snapshot.Span{Start: first, End: first + target.End - target.Start}, true
}

func gapOracle(original, current []oracleAtom, gap int) (int, bool) {
	at := positions(current)
	left, right := -1, len(current)
	leftOK, rightOK := true, true
	if gap > 0 {
		left, leftOK = at[original[gap-1].identity]
	}
	if gap < len(original) {
		right, rightOK = at[original[gap].identity]
	}
	return right, leftOK && rightOK && right == left+1
}

type oracleCase struct {
	original []oracleAtom
	states   [][]oracleAtom
	edits    []snapshot.Splice
}

func (c oracleCase) expectSpan(target snapshot.Span) (snapshot.Span, bool) {
	var mapped snapshot.Span
	for _, state := range c.states {
		span, ok := spanOracle(c.original, state, target)
		if !ok {
			return snapshot.Span{}, false
		}
		mapped = span
	}
	return mapped, true
}

func (c oracleCase) expectGap(gap int) (int, bool) {
	mapped := gap
	for _, state := range c.states {
		position, ok := gapOracle(c.original, state, gap)
		if !ok {
			return 0, false
		}
		mapped = position
	}
	return mapped, true
}

func (c oracleCase) mapSpan(target snapshot.Span) (snapshot.Span, bool) {
	mapped, ok := target, true
	for _, edit := range c.edits {
		if !ok {
			return snapshot.Span{}, false
		}
		mapped, ok = snapshot.MapSpan(mapped, edit)
	}
	return mapped, ok
}

func (c oracleCase) mapGap(gap int) (int, bool) {
	mapped, ok := gap, true
	for _, edit := range c.edits {
		if !ok {
			return 0, false
		}
		mapped, ok = snapshot.MapGap(mapped, edit)
	}
	return mapped, ok
}

func checkOracleCase(t *testing.T, c oracleCase, counts *oracleCounts) {
	t.Helper()
	for _, target := range oracleSpans(len(c.original)) {
		actual, accepted := c.mapSpan(target)
		expected, expectedOK := c.expectSpan(target)
		if accepted != expectedOK || accepted && actual != expected {
			t.Fatalf("target %+v edits %+v: got %+v/%v want %+v/%v", target, c.edits, actual, accepted, expected, expectedOK)
		}
		counts.addRange(accepted)
	}
	for gap := 0; gap <= len(c.original); gap++ {
		actual, accepted := c.mapGap(gap)
		expected, expectedOK := c.expectGap(gap)
		if accepted != expectedOK || accepted && actual != expected {
			t.Fatalf("gap %d edits %+v: got %d/%v want %d/%v", gap, c.edits, actual, accepted, expected, expectedOK)
		}
		counts.addGap(accepted)
	}
}

func TestHashlineSingleSpliceOracle(t *testing.T) {
	var counts oracleCounts
	for length := range 9 {
		original := oracleFile(length)
		for _, edit := range oracleSplices(length, 3) {
			current := oracleApply(original, edit, 100)
			checkOracleCase(t, oracleCase{original: original, states: [][]oracleAtom{current}, edits: []snapshot.Splice{edit}}, &counts)
		}
	}
	want := oracleCounts{rangeCases: 14178, rangeAccepted: 5676, gapCases: 4335, gapAccepted: 2400}
	if counts != want {
		t.Fatalf("single-splice counts %+v, want %+v", counts, want)
	}
}

func TestHashlineTwoSpliceOracle(t *testing.T) {
	var counts oracleCounts
	for length := range 6 {
		original := oracleFile(length)
		for _, first := range oracleSplices(length, 2) {
			intermediate := oracleApply(original, first, 100)
			for _, second := range oracleSplices(len(intermediate), 2) {
				current := oracleApply(intermediate, second, 200)
				checkOracleCase(t, oracleCase{original: original, states: [][]oracleAtom{intermediate, current}, edits: []snapshot.Splice{first, second}}, &counts)
			}
		}
	}
	want := oracleCounts{rangeCases: 62621, rangeAccepted: 17824, gapCases: 28539, gapAccepted: 9940}
	if counts != want {
		t.Fatalf("two-splice counts %+v, want %+v", counts, want)
	}
}

func TestHashlineEqualTextIsNotIdentity(t *testing.T) {
	original := []oracleAtom{{0, "fn live"}, {1, "return 1;"}, {2, "fn fixture"}, {3, "return 2;"}}
	current := []oracleAtom{{0, "fn live"}, {4, "return 3;"}, {2, "fn fixture"}, {5, "return 1;"}}
	if _, ok := spanOracle(original, current, snapshot.Span{Start: 1, End: 2}); ok {
		t.Fatal("equal text established original identity")
	}
}

func TestHashlineRangeBoundaryInsertions(t *testing.T) {
	target := snapshot.Span{Start: 1, End: 3}
	cases := []struct {
		edit snapshot.Splice
		want snapshot.Span
		ok   bool
	}{
		{snapshot.Splice{Start: 1, End: 1, Inserted: 2}, snapshot.Span{Start: 3, End: 5}, true},
		{snapshot.Splice{Start: 3, End: 3, Inserted: 2}, target, true},
		{snapshot.Splice{Start: 2, End: 2, Inserted: 2}, snapshot.Span{}, false},
	}
	for _, test := range cases {
		got, ok := snapshot.MapSpan(target, test.edit)
		if ok != test.ok || got != test.want {
			t.Fatalf("edit %+v: got %+v/%v want %+v/%v", test.edit, got, ok, test.want, test.ok)
		}
	}
}

func TestHashlineGapNeighborsAreProtected(t *testing.T) {
	for _, edit := range []snapshot.Splice{{Start: 2, End: 2, Inserted: 1}, {Start: 1, End: 2, Inserted: 1}, {Start: 2, End: 3, Inserted: 1}} {
		if _, ok := snapshot.MapGap(2, edit); ok {
			t.Fatalf("gap survived %+v", edit)
		}
	}
	for _, gap := range []int{0, 4} {
		if _, ok := snapshot.MapGap(gap, snapshot.Splice{Start: gap, End: gap, Inserted: 1}); ok {
			t.Fatalf("boundary gap %d survived", gap)
		}
	}
}
