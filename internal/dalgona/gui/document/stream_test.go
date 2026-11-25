package document

import (
	"math/rand/v2"
	"reflect"
	"strings"
	"testing"
)

func openMarkdown(t *testing.T, item, source string) *Document {
	t.Helper()
	d, err := New(Header{Identity: committed(t, item), Format: FormatMarkdown, Open: true}, source)
	if err != nil {
		t.Fatal(err)
	}
	return d
}

func appendAll(t *testing.T, d *Document, chunks ...string) *Document {
	t.Helper()
	for _, chunk := range chunks {
		next, change, err := d.Append(chunk)
		if err != nil {
			t.Fatal(err)
		}
		if !change.Appended || change.Revision != d.Revision()+1 || next.Revision() != change.Revision {
			t.Fatalf("append change %+v from revision %d", change, d.Revision())
		}
		d = next
	}
	return d
}

func blockShape(d *Document) []Block {
	shape := make([]Block, d.Len())
	for i := range d.Len() {
		b := d.Block(i)
		b.Key = Key{}
		for c := range b.Containers {
			b.Containers[c].id = 0
		}
		shape[i] = b
	}
	return shape
}

func splitRandomly(r *rand.Rand, source string) []string {
	var chunks []string
	for source != "" {
		n := 1 + r.IntN(12)
		for n < len(source) && !utf8Start(source[n]) {
			n++
		}
		n = min(n, len(source))
		chunks = append(chunks, source[:n])
		source = source[n:]
	}
	return chunks
}

func utf8Start(b byte) bool {
	return b&0xC0 != 0x80
}

func TestStreamingConvergesToTheFullParse(t *testing.T) {
	for seed := range uint64(150) {
		r := rand.New(rand.NewPCG(seed, 0xa99e))
		source := randomMarkdown(r)
		streamed := appendAll(t, openMarkdown(t, "stream", ""), splitRandomly(r, source)...)
		closed, _, err := streamed.Close()
		if err != nil {
			t.Fatal(err)
		}
		whole := markdown(t, "stream", source)
		if !reflect.DeepEqual(blockShape(closed), blockShape(whole)) {
			t.Fatalf("seed %d: streamed blocks differ from the full parse of %q", seed, source)
		}
		for i := range whole.Len() {
			if closed.Block(i).Key != whole.Block(i).Key {
				t.Fatalf("seed %d: block %d key %+v, full parse key %+v", seed, i, closed.Block(i).Key, whole.Block(i).Key)
			}
		}
		checkProjection(t, closed)
	}
}

func TestLaterDefinitionResolvesEarlierReference(t *testing.T) {
	d := appendAll(t, openMarkdown(t, "refs", "see [docs] now\n\nmiddle\n\n"), "[docs]: https://docs.example \"Docs\"\n")
	first := d.Block(0)
	if first.Projection.Text != "see docs now" || len(first.Links) != 1 || first.Links[0].Destination != "https://docs.example" {
		t.Fatalf("earlier reference stayed unresolved: %q %+v", first.Projection.Text, first.Links)
	}
	d = appendAll(t, d, "\n[docs]: https://ignored.example\n")
	if got := d.Block(0).Links[0].Destination; got != "https://docs.example" {
		t.Fatalf("second definition replaced the first: %q", got)
	}
}

func TestAppendKeepsClosedPositionsAndRepairsTheTail(t *testing.T) {
	d := openMarkdown(t, "tail", "First paragraph.\n\nHello e")
	stable, err := d.Position(strings.Index(d.Source(), "paragraph"), After)
	if err != nil {
		t.Fatal(err)
	}
	end, _ := d.End()
	start, _ := d.Position(strings.Index(d.Source(), "Hello"), After)
	next, change, appendErr := d.Append("\u0301 world")
	if appendErr != nil {
		t.Fatal(appendErr)
	}
	checkStablePositionSurvivesAppend(t, next, change, stable)
	mappedEnd := checkTailEndRepaired(t, next, change, end)
	checkRepairedSelection(t, next, change, start, mappedEnd)
}

func checkStablePositionSurvivesAppend(t *testing.T, next *Document, change Change, stable Position) {
	t.Helper()
	mappedStable, err := next.Remap(change, stable)
	if err != nil || mappedStable.Offset != stable.Offset || mappedStable.Block != stable.Block {
		t.Fatalf("closed position moved: %+v -> %+v (%v)", stable, mappedStable, err)
	}
}

func checkTailEndRepaired(t *testing.T, next *Document, change Change, end Position) Position {
	t.Helper()
	mappedEnd, err := next.Remap(change, end)
	if err != nil {
		t.Fatal(err)
	}
	if want := strings.Index(next.Source(), " world"); mappedEnd.Offset != want {
		t.Fatalf("tail repair put the end at %d, want %d after the combined cluster", mappedEnd.Offset, want)
	}
	return mappedEnd
}

func checkRepairedSelection(t *testing.T, next *Document, change Change, start, mappedEnd Position) {
	t.Helper()
	mappedStart, _ := next.Remap(change, start)
	text, err := next.Text(mappedStart, mappedEnd)
	if err != nil || text != "Hello e\u0301" {
		t.Fatalf("repaired selection copies %q (%v)", text, err)
	}
}

func TestAfterAffinityAtTheTailMovesBeforeTheGrownCluster(t *testing.T) {
	d := openMarkdown(t, "after", "ab")
	p, err := d.Position(1, After)
	if err != nil {
		t.Fatal(err)
	}
	next, change, err := d.Append("\u0308")
	if err != nil {
		t.Fatal(err)
	}
	if mapped, _ := next.Remap(change, p); mapped.Offset != 1 {
		t.Fatalf("after-affinity position moved to %d", mapped.Offset)
	}
	q, _ := d.Position(2, After)
	if mapped, _ := next.Remap(change, q); mapped.Offset != 1 {
		t.Fatalf("after-affinity end did not move to the start of the grown cluster: %d", mapped.Offset)
	}
}

func TestTailKeysStayProvisionalUntilTheBlockCloses(t *testing.T) {
	d := openMarkdown(t, "keys", "one\n\ntwo")
	if !d.Block(1).Key.Provisional || d.Block(0).Key.Provisional {
		t.Fatalf("keys %+v %+v", d.Block(0).Key, d.Block(1).Key)
	}
	provisional := d.Block(1).Key
	state := NewState(NewScope("local", "thread-1"))
	if _, err := state.Scope.Put(d); err != nil {
		t.Fatal(err)
	}
	state.Pending[provisional] = 7
	p, _ := d.Position(strings.Index(d.Source(), "two"), After)
	state.Selection, state.Selected = Caret(p), true

	next, change, appendErr := d.Append(" more\n\nthree")
	if appendErr != nil {
		t.Fatal(appendErr)
	}
	if publishErr := state.Publish(change, next); publishErr != nil {
		t.Fatal(publishErr)
	}
	checkTailBlockClosed(t, state, next, tailExpect{p: p, provisional: provisional, prev: d.Revision()})
	checkTailClose(t, next)
}

type tailExpect struct {
	p           Position
	provisional Key
	prev        Revision
}

func checkTailBlockClosed(t *testing.T, state *State, next *Document, want tailExpect) Key {
	t.Helper()
	final := next.Block(1).Key
	checkClosedKeyFinal(t, final, next)
	checkPendingCarried(t, state, final)
	checkTailSelection(t, state, final, want.p)
	checkRevisionGating(t, state, final, next.Revision(), want)
	return final
}

func checkClosedKeyFinal(t *testing.T, final Key, next *Document) {
	t.Helper()
	if final.Provisional || final.Span != next.Block(1).Span || final.Kind != KindParagraph || final.Hash == 0 {
		t.Fatalf("closed block key %+v", final)
	}
}

func checkPendingCarried(t *testing.T, state *State, final Key) {
	t.Helper()
	if state.Pending[final] != 7 || len(state.Pending) != 1 {
		t.Fatalf("pending renderer references %+v, want job 7 under %+v", state.Pending, final)
	}
}

func checkTailSelection(t *testing.T, state *State, final Key, p Position) {
	t.Helper()
	if state.Selection.Anchor.Block != final || state.Selection.Anchor.Offset != p.Offset {
		t.Fatalf("selection %+v was not remapped to %+v", state.Selection.Anchor, final)
	}
}

func checkRevisionGating(t *testing.T, state *State, final Key, rev Revision, want tailExpect) {
	t.Helper()
	if !state.Current(final, rev) || state.Current(final, want.prev) || state.Current(want.provisional, rev) {
		t.Fatal("renderer results are not checked against revision and key")
	}
}

func checkTailClose(t *testing.T, next *Document) {
	t.Helper()
	closed, change, err := next.Close()
	if err != nil {
		t.Fatal(err)
	}
	if len(change.Keys) != 1 || !change.Keys[0].From.Provisional || closed.Block(2).Key.Provisional {
		t.Fatalf("close changes %+v", change.Keys)
	}
}

func TestPublishRejectsAnotherThread(t *testing.T) {
	state := NewState(NewScope("local", "thread-1"))
	d := openMarkdown(t, "x", "text")
	if _, err := state.Scope.Put(d); err != nil {
		t.Fatal(err)
	}
	other, _ := Committed("local", "thread-2", "turn-1", "x")
	foreign, _ := New(Header{Identity: other, Format: FormatMarkdown}, "other")
	if _, err := state.Scope.Put(foreign); err == nil {
		t.Fatal("scope accepted a document from another thread")
	}
	next, change, _ := d.Append(" more")
	change.To = other
	if err := state.Publish(change, next); err == nil {
		t.Fatal("publish accepted a change for another identity")
	}
	if doc, _ := state.Scope.Loaded(0); doc.Source() != "text" {
		t.Fatalf("rejected change altered the document: %q", doc.Source())
	}
}

func TestReplaceMapsSurvivingAndRemovedPositions(t *testing.T) {
	d := markdown(t, "replace", "keep one\n\ndrop this\n\nkeep two")
	one, _ := d.Position(strings.Index(d.Source(), "one"), After)
	two, _ := d.Position(strings.Index(d.Source(), "two"), After)
	removedStart, _ := d.Position(strings.Index(d.Source(), "this"), After)
	removedEnd, _ := d.Position(strings.Index(d.Source(), "this")+3, Before)
	state := NewState(NewScope("local", "thread-1"))
	if _, err := state.Scope.Put(d); err != nil {
		t.Fatal(err)
	}
	state.Selection, state.Selected = Selection{Anchor: removedStart, Focus: removedEnd}, true
	state.Anchor, state.Anchored = Anchor{Position: one, Displacement: -4}, true

	next, change, replaceErr := d.Replace("intro\n\nkeep one\n\nkeep two")
	if replaceErr != nil {
		t.Fatal(replaceErr)
	}
	if publishErr := state.Publish(change, next); publishErr != nil {
		t.Fatal(publishErr)
	}
	checkSurvivingPositions(t, state, next, change, two)
	checkRemovedPositions(t, next, change, removedStart, removedEnd)
	checkMovedBlockRemapped(t, d, next, change)
}

func checkSurvivingPositions(t *testing.T, state *State, next *Document, change Change, two Position) {
	t.Helper()
	if got := state.Anchor.Position.Offset; got != strings.Index(next.Source(), "one") {
		t.Fatalf("scroll anchor inside surviving text moved to %d", got)
	}
	if mapped, _ := next.Remap(change, two); mapped.Offset != strings.Index(next.Source(), "two") {
		t.Fatalf("surviving position moved to %d", mapped.Offset)
	}
	if text, _ := next.Text(state.Selection.Anchor, state.Selection.Focus); text != "" {
		t.Fatalf("selection inside removed text now selects %q", text)
	}
}

func checkRemovedPositions(t *testing.T, next *Document, change Change, removedStart, removedEnd Position) {
	t.Helper()
	before, _ := next.Remap(change, removedEnd)
	after, _ := next.Remap(change, removedStart)
	if before.Offset != len("intro\n\nkeep one") || after.Offset != strings.Index(next.Source(), "keep two") {
		t.Fatalf("removed positions moved to %d and %d", before.Offset, after.Offset)
	}
}

func checkMovedBlockRemapped(t *testing.T, d, next *Document, change Change) {
	t.Helper()
	found := false
	for _, k := range change.Keys {
		if strings.HasPrefix(d.Source()[k.From.Span.Start:], "keep two") && strings.HasPrefix(next.Source()[k.To.Span.Start:], "keep two") {
			found = true
		}
	}
	if !found {
		t.Fatalf("moved block key was not remapped: %+v", change.Keys)
	}
}
