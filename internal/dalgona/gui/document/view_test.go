package document

import (
	"fmt"
	"math"
	"strings"
	"testing"
)

const rowHeight = 20

func fixedRows(RowRef, *Scope) float32 { return rowHeight }

func transcript(t *testing.T, count int) *Scope {
	t.Helper()
	scope := NewScope("local", "thread-1")
	for i := range count {
		d := markdown(t, fmt.Sprintf("item-%d", i), fmt.Sprintf("message %d first\n\nmessage %d second\n\n```\nline a\nline b\n```\n", i, i))
		if _, err := scope.Put(d); err != nil {
			t.Fatal(err)
		}
	}
	return scope
}

func rowStart(t *testing.T, scope *Scope, ref RowRef) Position {
	t.Helper()
	doc, _ := scope.Loaded(ref.Entry)
	b := doc.Block(ref.Block)
	return doc.at(ref.Block, b.Rows[ref.Row].Start, After)
}

func atRowTop(int, Position) float32 { return 0 }

func TestAnchorKeepsItsSourcePositionWhenContentArrivesAbove(t *testing.T) {
	scope := transcript(t, 6)
	v := NewViewport()
	v.Sync(scope, fixedRows)
	v.Resize(100)
	v.JumpToLatest()
	v.ScrollBy(-150)
	if v.Following() {
		t.Fatal("scrolling 150 pixels up kept following")
	}
	row := v.RowAt(v.Offset())
	anchor := v.Capture(row, rowStart(t, scope, v.Row(row)), 0)
	anchored := v.Row(row)
	anchoredDoc := scope.Ref(anchored.Entry).Header.Identity

	older := []Ref{
		{Header: Header{Identity: committed(t, "older-1"), Format: FormatMarkdown}, Revision: 1, Length: 900},
		{Header: Header{Identity: committed(t, "older-2"), Format: FormatMarkdown}, Revision: 1, Length: 400},
	}
	if err := scope.Prepend(older...); err != nil {
		t.Fatal(err)
	}
	v.Sync(scope, fixedRows)
	v.Restore(scope, anchor, atRowTop)
	assertAnchor(t, v, scope, anchorExpect{anchor: anchor, doc: anchoredDoc, ref: anchored})

	for i := range 3 {
		v.Measure(i, 75)
	}
	v.Restore(scope, anchor, atRowTop)
	assertAnchor(t, v, scope, anchorExpect{anchor: anchor, doc: anchoredDoc, ref: anchored})

	state := NewState(scope)
	state.Anchor, state.Anchored = anchor, true
	e, _ := scope.Index(anchoredDoc)
	doc, _ := scope.Loaded(e)
	next, change, err := doc.Replace("inserted above\n\n" + doc.Source())
	if err != nil {
		t.Fatal(err)
	}
	if err := state.Publish(change, next); err != nil {
		t.Fatal(err)
	}
	if want := anchor.Position.Offset + len("inserted above\n\n"); state.Anchor.Position.Offset != want {
		t.Fatalf("anchor offset %d, want %d", state.Anchor.Position.Offset, want)
	}
	v.Sync(scope, fixedRows)
	v.Restore(scope, state.Anchor, atRowTop)
	located, _ := v.Locate(scope, state.Anchor.Position)
	if got := v.RowTop(located) - v.Offset(); math.Abs(got-float64(anchor.Displacement)) > 1 {
		t.Fatalf("anchor displacement %v, want %v", got, anchor.Displacement)
	}
}

type anchorExpect struct {
	anchor Anchor
	doc    Identity
	ref    RowRef
}

func assertAnchor(t *testing.T, v *Viewport, scope *Scope, want anchorExpect) {
	t.Helper()
	row, ok := v.Locate(scope, want.anchor.Position)
	if !ok {
		t.Fatal("anchor position is not in the viewport")
	}
	got := v.Row(row)
	if scope.Ref(got.Entry).Header.Identity != want.doc || got.Block != want.ref.Block || got.Row != want.ref.Row {
		t.Fatalf("anchor now in row %+v, want %+v of %+v", got, want.ref, want.doc)
	}
	if displacement := v.RowTop(row) - v.Offset(); math.Abs(displacement-float64(want.anchor.Displacement)) > 1 {
		t.Fatalf("anchor displacement %v, want %v", displacement, want.anchor.Displacement)
	}
}

func TestFollowingUsesTheThirtyTwoPixelThreshold(t *testing.T) {
	scope := transcript(t, 3)
	v := NewViewport()
	v.Sync(scope, fixedRows)
	v.Resize(100)
	v.Restore(scope, Anchor{}, atRowTop)
	checkNewViewportFollows(t, v)
	checkThresholdBoundary(t, v)
	checkUnfollowOnOutput(t, v, scope)
	checkJumpToLatest(t, v)
	checkFollowAfterGrowth(t, v, scope)
}

func checkNewViewportFollows(t *testing.T, v *Viewport) {
	t.Helper()
	if !v.Following() || v.Offset() != v.Total()-100 {
		t.Fatalf("new viewport offset %v of %v", v.Offset(), v.Total())
	}
}

func checkThresholdBoundary(t *testing.T, v *Viewport) {
	t.Helper()
	v.ScrollBy(-FollowDistance)
	if !v.Following() {
		t.Fatal("32 pixels from the end stopped following")
	}
	v.ScrollBy(-1)
	if v.Following() {
		t.Fatal("33 pixels from the end kept following")
	}
}

func checkUnfollowOnOutput(t *testing.T, v *Viewport, scope *Scope) {
	t.Helper()
	offset := v.Offset()
	d := markdown(t, "new", "new output\n\nwith rows")
	if _, err := scope.Put(d); err != nil {
		t.Fatal(err)
	}
	v.Sync(scope, fixedRows)
	v.Restore(scope, Anchor{Position: rowStart(t, scope, v.Row(v.RowAt(offset))), Displacement: float32(v.RowTop(v.RowAt(offset)) - offset)}, atRowTop)
	if v.Following() || v.Unseen() != 1 || v.Offset() != offset {
		t.Fatalf("new output moved a reader: following %v unseen %d offset %v want %v", v.Following(), v.Unseen(), v.Offset(), offset)
	}
}

func checkJumpToLatest(t *testing.T, v *Viewport) {
	t.Helper()
	v.JumpToLatest()
	if !v.Following() || v.Unseen() != 0 || v.Offset() != v.Total()-100 {
		t.Fatalf("jump to latest: following %v unseen %d offset %v", v.Following(), v.Unseen(), v.Offset())
	}
}

func checkFollowAfterGrowth(t *testing.T, v *Viewport, scope *Scope) {
	t.Helper()
	before := v.Total()
	more, _ := New(Header{Identity: committed(t, "more"), Format: FormatMarkdown}, strings.Repeat("line\n\n", 20))
	if _, err := scope.Put(more); err != nil {
		t.Fatal(err)
	}
	v.Sync(scope, fixedRows)
	v.Restore(scope, Anchor{}, atRowTop)
	if !v.Following() || v.Total() <= before || v.Offset() != v.Total()-100 {
		t.Fatalf("following reader left the end after %v pixels of output", v.Total()-before)
	}
}

func TestWindowLaysOutAtMostOneViewportOfOverscan(t *testing.T) {
	scope := transcript(t, 40)
	v := NewViewport()
	v.Sync(scope, fixedRows)
	v.Resize(200)
	v.JumpToLatest()
	v.ScrollBy(-1000)
	first, last := v.Window()
	top := v.Offset()
	if v.RowTop(first) > top-200 || v.RowTop(first)+rowHeight <= top-200 {
		t.Fatalf("first row %d at %v for viewport top %v", first, v.RowTop(first), top)
	}
	if v.RowTop(last-1) >= top+400 || v.RowTop(last) < top+400 {
		t.Fatalf("last row %d at %v for viewport bottom %v", last, v.RowTop(last), top+200)
	}
}

func TestDragScrollIsBounded(t *testing.T) {
	scope := transcript(t, 40)
	v := NewViewport()
	v.Sync(scope, fixedRows)
	v.Resize(200)
	v.ScrollBy(0)
	start := v.Offset()
	v.DragScroll(1e6, 0.5)
	if moved := v.Offset() - start; moved != MaxDragScrollSpeed*0.5 {
		t.Fatalf("drag scroll moved %v", moved)
	}
	v.DragScroll(100, 0.5)
	if v.Offset()-start != MaxDragScrollSpeed*0.5 {
		t.Fatal("a pointer inside the viewport scrolled")
	}
}

func TestGeometryCacheReusesClosedBlocksAndRespectsBudget(t *testing.T) {
	d := openMarkdown(t, "cache", "closed paragraph\n\ntail")
	next := appendAll(t, d, " grows")
	checkClosedBlockKeyStable(t, d, next)
	checkCacheEvictionAndBudget(t)
}

func checkClosedBlockKeyStable(t *testing.T, d, next *Document) {
	t.Helper()
	if NewGeometryKey(d, 0, 0) != NewGeometryKey(next, 0, 0) {
		t.Fatal("appending to the tail invalidated the closed block geometry")
	}
	if NewGeometryKey(d, 1, 0) == NewGeometryKey(next, 1, 0) {
		t.Fatal("the growing tail reused stale geometry")
	}
}

func checkCacheEvictionAndBudget(t *testing.T) {
	t.Helper()
	cache := NewCache[string](100)
	keys := make([]GeometryKey, 4)
	for i := range keys {
		keys[i] = GeometryKey{Row: i, Width: 640}
	}
	cache.Put(keys[0], "a", 40)
	cache.Put(keys[1], "b", 40)
	cache.Get(keys[0])
	cache.Put(keys[2], "c", 40)
	checkEvictedEntry(t, cache, keys)
	checkBudgetEnforced(t, cache, keys)
}

func checkEvictedEntry(t *testing.T, cache *Cache[string], keys []GeometryKey) {
	t.Helper()
	if _, ok := cache.Get(keys[1]); ok {
		t.Fatal("least recently used entry survived")
	}
	if _, ok := cache.Get(keys[0]); !ok || cache.Used() != 80 {
		t.Fatalf("recent entry evicted or usage %d", cache.Used())
	}
}

func checkBudgetEnforced(t *testing.T, cache *Cache[string], keys []GeometryKey) {
	t.Helper()
	if cache.Put(keys[3], "huge", 101) || cache.Used() > 100 {
		t.Fatal("entry above the budget admitted")
	}
	wider := keys[0]
	wider.Width = 800
	if _, ok := cache.Get(wider); ok {
		t.Fatal("a different width reused geometry")
	}
}
