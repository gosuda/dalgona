package snapshot

import (
	"encoding/json/jsontext"
	"errors"
	"strconv"
	"strings"
	"testing"
)

func TestBuffersStayImmutable(t *testing.T) {
	store := New()
	body := []byte("alpha\n")
	ref := store.Put("f.txt", body, []Range{{Start: 1, End: 1}})
	body[0] = 'Z'
	got, err := store.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	if string(got.Data) != "alpha\n" {
		t.Fatalf("capture followed caller mutation %q", got.Data)
	}
	got.Data[0] = 'Q'
	again, err := store.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	if string(again.Data) != "alpha\n" {
		t.Fatal("get returned a live buffer")
	}
}

func TestShownStopsAtCapturedLines(t *testing.T) {
	store := New()
	ref := store.Put("f.txt", []byte("a\n"), []Range{{Start: 1, End: 1}, {Start: 5, End: 9}})
	if store.Covers(ref, 5, 9) || !store.Covers(ref, 1, 1) {
		t.Fatal("shown coverage escaped the captured file")
	}
}

func TestRestoreRebuildsHeadAndCounter(t *testing.T) {
	store := New()
	data := []byte("one\ntwo\n")
	ref := store.Put("f.txt", data, []Range{{Start: 1, End: 1}})
	entry, err := store.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	rebuilt := New()
	if err := rebuilt.Restore(entry.Meta(), data); err != nil {
		t.Fatal(err)
	}
	if !rebuilt.Unchanged(ref, "f.txt", data) || !rebuilt.Covers(ref, 1, 1) {
		t.Fatal("rebuilt store lost head or shown ranges")
	}
	boot, _, _ := strings.Cut(strings.TrimPrefix(ref, "r"), ".")
	high := "r" + boot + "." + strconv.FormatUint(100, 36)
	highEntry := Entry{Ref: high, Path: "f.txt", Blob: Digest(data), Shown: []Range{{Start: 1, End: 2}}, Revision: 4, Epoch: 2}
	if err := rebuilt.Restore(highEntry.Meta(), data); err != nil {
		t.Fatal(err)
	}
	if rebuilt.Unchanged(ref, "f.txt", data) || !rebuilt.Unchanged(high, "f.txt", data) {
		t.Fatal("older metadata replaced a newer head")
	}
	next := rebuilt.Put("f.txt", data, nil)
	if refSequence(next) <= 100 {
		t.Fatalf("counter reused after restore: %s", next)
	}
}

func TestRestoreKeepsNewerHeadWhenOlderArrivesLast(t *testing.T) {
	data := []byte("v\n")
	older := Entry{Ref: "r0123456789abcdef0123456789abcdef.1", Path: "f.txt", Blob: Digest(data), Shown: []Range{{Start: 1, End: 1}}, Revision: 1, Epoch: 1}
	newer := older
	newer.Ref = "r0123456789abcdef0123456789abcdef.2"
	newer.Revision = 3
	store := New()
	if err := store.Restore(newer.Meta(), data); err != nil {
		t.Fatal(err)
	}
	if err := store.Restore(older.Meta(), data); err != nil {
		t.Fatal(err)
	}
	if !store.Unchanged(newer.Ref, "f.txt", data) || store.Unchanged(older.Ref, "f.txt", data) {
		t.Fatal("head moved backward")
	}
	if err := store.Restore(older.Meta(), data); err != nil {
		t.Fatal(err)
	}
}

func TestRestoreRejectsConflictingGeneration(t *testing.T) {
	left := []byte("left\n")
	right := []byte("right\n")
	store := New()
	first := Entry{Ref: "r0123456789abcdef0123456789abcdef.1", Path: "f.txt", Blob: Digest(left), Revision: 1, Epoch: 1}
	second := Entry{Ref: "r0123456789abcdef0123456789abcdef.2", Path: "f.txt", Blob: Digest(right), Revision: 1, Epoch: 1}
	if err := store.Restore(first.Meta(), left); err != nil {
		t.Fatal(err)
	}
	if err := store.Restore(second.Meta(), right); err == nil {
		t.Fatal("accepted two blobs for one revision")
	}
	if !store.Unchanged(first.Ref, "f.txt", left) {
		t.Fatal("conflict replaced the stored head")
	}
}

func TestDeletedHeadRejectsOlderRestore(t *testing.T) {
	store := New()
	data := []byte("body\n")
	ref := store.Put("f.txt", data, []Range{{Start: 1, End: 1}})
	entry, err := store.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	store.Remove("f.txt", []byte("other"))
	if err := store.Restore(entry.Meta(), data); err != nil {
		t.Fatal(err)
	}
	if store.Unchanged(ref, "f.txt", data) {
		t.Fatal("older metadata resurrected a deleted head")
	}
}

func TestRestoreRejectsHostileMetadata(t *testing.T) {
	data := []byte("a\n")
	blob := Digest(data)
	ref := "r0123456789abcdef0123456789abcdef.1"
	good := `{"ref":"` + ref + `","path":"f.txt","blob":"` + blob + `","shown":[[1,1]],"revision":1,"epoch":1}`
	hostile := []jsontext.Value{
		jsontext.Value(good + `{}`),
		jsontext.Value(`{"ref":"` + ref + `","ref":"` + ref + `","path":"f.txt","blob":"` + blob + `","shown":[],"revision":1,"epoch":1}`),
		jsontext.Value(`{"ref":"` + ref + `","path":"f.txt","blob":"` + blob + `","shown":[[1,1]],"revision":1,"epoch":1,"extra":true}`),
		jsontext.Value(`{"ref":"` + ref + `","path":"f.txt","blob":"` + blob + `","shown":[[5,5]],"revision":1,"epoch":1}`),
		jsontext.Value(`{"ref":"` + ref + `","path":"f.txt","blob":"` + blob + `","shown":[[1,1,1]],"revision":1,"epoch":1}`),
		jsontext.Value(`{"path":"f.txt","blob":"` + blob + `","shown":[],"revision":1,"epoch":1}`),
		jsontext.Value(`{"ref":"` + ref + `","path":"f.txt","blob":"` + blob + `","shown":[[1,1]],"revision":0,"epoch":1}`),
		jsontext.Value(`{"ref":"` + ref + `","path":"f.txt","blob":"` + blob + `","shown":[[1,1]],"revision":"1","epoch":1}`),
		jsontext.Value(`{"ref":"nope","path":"f.txt","blob":"` + blob + `","shown":[],"revision":1,"epoch":1}`),
	}
	for _, meta := range hostile {
		if err := New().Restore(meta, data); err == nil {
			t.Fatalf("accepted hostile metadata %s", meta)
		}
	}
	if err := New().Restore(jsontext.Value(good), []byte("b\n")); err == nil {
		t.Fatal("accepted blob that does not match data")
	}
}

func TestSpanLineageShiftsOnlyAtStart(t *testing.T) {
	store := New()
	before := []byte("a\nb\n")
	ref := store.Put("f.txt", before, nil)
	shifted := []byte("Z\na\nb\n")
	if store.Record("f.txt", before, shifted, []Splice{{Start: 0, End: 0, Inserted: 1}}) == "" {
		t.Fatal("record failed")
	}
	mapped, err := store.ResolveSpan(ref, "f.txt", shifted, Span{Start: 0, End: 1})
	if err != nil || mapped != (Span{Start: 1, End: 2}) {
		t.Fatalf("insertion at start: %+v %v", mapped, err)
	}
	wide, err := store.ResolveSpan(ref, "f.txt", shifted, Span{Start: 0, End: 2})
	if err != nil || wide != (Span{Start: 1, End: 3}) {
		t.Fatalf("insertion expanded span: %+v %v", wide, err)
	}
	after := []byte("a\nZ\nb\n")
	store = New()
	ref = store.Put("f.txt", before, nil)
	store.Record("f.txt", before, after, []Splice{{Start: 1, End: 1, Inserted: 1}})
	end, err := store.ResolveSpan(ref, "f.txt", after, Span{Start: 0, End: 1})
	if err != nil || end != (Span{Start: 0, End: 1}) {
		t.Fatalf("insertion at end changed span: %+v %v", end, err)
	}
}

func TestSpanAndGapConflicts(t *testing.T) {
	before := []byte("a\nb\n")
	inserted := []byte("a\nZ\nb\n")
	store := New()
	ref := store.Put("f.txt", before, nil)
	store.Record("f.txt", before, inserted, []Splice{{Start: 1, End: 1, Inserted: 1}})
	if _, err := store.ResolveSpan(ref, "f.txt", inserted, Span{Start: 0, End: 2}); !errors.Is(err, ErrTargetChanged) {
		t.Fatalf("inside insertion err %v", err)
	}
	replaced := []byte("Z\nb\n")
	store = New()
	ref = store.Put("f.txt", before, nil)
	store.Record("f.txt", before, replaced, []Splice{{Start: 0, End: 1, Inserted: 1}})
	if _, err := store.ResolveSpan(ref, "f.txt", replaced, Span{Start: 0, End: 2}); !errors.Is(err, ErrTargetChanged) {
		t.Fatalf("overlap err %v", err)
	}
	if _, err := store.ResolveGap(ref, "f.txt", replaced, 1); !errors.Is(err, ErrGapChanged) {
		t.Fatalf("neighbor err %v", err)
	}
	store = New()
	ref = store.Put("f.txt", before, nil)
	prefix := []byte("Z\na\nb\n")
	store.Record("f.txt", before, prefix, []Splice{{Start: 0, End: 0, Inserted: 1}})
	if _, err := store.ResolveGap(ref, "f.txt", prefix, 0); !errors.Is(err, ErrGapChanged) {
		t.Fatalf("same gap err %v", err)
	}
}

func TestUndoDoesNotReviveConflict(t *testing.T) {
	store := New()
	before := []byte("a\nb\nc\n")
	ref := store.Put("f.txt", before, nil)
	mid := []byte("a\nX\nc\n")
	store.Record("f.txt", before, mid, []Splice{{Start: 1, End: 2, Inserted: 1}})
	store.Record("f.txt", mid, before, []Splice{{Start: 1, End: 2, Inserted: 1}})
	_, err := store.ResolveSpan(ref, "f.txt", before, Span{Start: 1, End: 2})
	if !errors.Is(err, ErrTargetChanged) {
		t.Fatalf("undo revived conflict: %v", err)
	}
}

func TestMissingHistoryEndsLineage(t *testing.T) {
	store := New()
	before := []byte("a\n")
	ref := store.Put("f.txt", before, nil)
	_, err := store.ResolveSpan(ref, "f.txt", []byte("b\n"), Span{Start: 0, End: 1})
	if !errors.Is(err, ErrStaleReference) {
		t.Fatalf("external change err %v", err)
	}
	newer := []byte("c\n")
	newerMeta := Entry{
		Ref: "r0123456789abcdef0123456789abcdef.a", Path: "f.txt", Blob: Digest(newer),
		Revision: 9, Epoch: 3,
	}.Meta()
	if restoreErr := store.Restore(newerMeta, newer); restoreErr != nil {
		t.Fatal(restoreErr)
	}
	_, err = store.ResolveSpan(ref, "f.txt", newer, Span{Start: 0, End: 1})
	if !errors.Is(err, ErrStaleReference) {
		t.Fatalf("missing edge err %v", err)
	}
}

func TestUnsoundSpliceEndsLineage(t *testing.T) {
	store := New()
	before := []byte("a\nb\n")
	ref := store.Put("f.txt", before, nil)
	after := []byte("a\n")
	store.Record("f.txt", before, after, []Splice{{Start: 2, End: 1, Inserted: 0}})
	_, err := store.ResolveSpan(ref, "f.txt", after, Span{Start: 0, End: 1})
	if !errors.Is(err, ErrStaleReference) {
		t.Fatalf("unsound splice stayed usable: %v", err)
	}
}
