package snapshot

import (
	"errors"
	"slices"
	"strconv"
	"strings"
	"testing"
)

func TestReferenceGrammar(t *testing.T) {
	valid := "r0123456789abcdef0123456789abcdef.1a"
	if !ValidRef(valid) {
		t.Fatal("valid reference rejected")
	}
	invalid := []string{"", "r", "rabc.1", "r0123456789abcdef0123456789abcdef.0", "r0123456789abcdef0123456789abcdef.01", "r0123456789abcdef0123456789ABCDEF.1", "r0123456789abcdef0123456789abcdef.zzzzzzzzzzzzz"}
	store := New()
	for _, ref := range invalid {
		if ValidRef(ref) {
			t.Fatalf("accepted %q", ref)
		}
		if _, err := store.Get(ref); !errors.Is(err, ErrUnknownReference) {
			t.Fatalf("malformed %q classified as %v", ref, err)
		}
	}
	if _, err := store.Get(valid); !errors.Is(err, ErrUnknownReference) {
		t.Fatalf("well-formed reference of another boot classified as %v", err)
	}
}

func TestCounterExhaustionDoesNotWrap(t *testing.T) {
	store := New()
	store.seq = ^uint64(0)
	if ref := store.Put("f.txt", []byte("body"), nil); ref != "" {
		t.Fatalf("exhausted counter issued %q", ref)
	}
}

func TestEvictionNeverReusesReference(t *testing.T) {
	store := New()
	first := store.Put("f.txt", []byte("body"), nil)
	seen := map[string]bool{first: true}
	last := first
	for range maxEntries {
		last = store.Put("f.txt", []byte("body"), nil)
		if seen[last] {
			t.Fatalf("reference reused: %s", last)
		}
		seen[last] = true
	}
	if _, err := store.Get(first); !errors.Is(err, ErrExpiredReference) || errors.Is(err, ErrUnknownReference) {
		t.Fatalf("old reference error %v", err)
	}
	if _, err := store.Get(last); err != nil {
		t.Fatalf("newest reference error %v", err)
	}
}

func TestEqualBytesHaveDistinctReferences(t *testing.T) {
	store := New()
	first := store.Put("f.txt", []byte("same"), nil)
	second := store.Put("f.txt", []byte("same"), nil)
	if first == second {
		t.Fatal("equal captures share a reference")
	}
	left, _ := store.Get(first)
	right, _ := store.Get(second)
	if left.Revision != right.Revision || left.Epoch != right.Epoch {
		t.Fatalf("unchanged capture advanced lineage: %+v %+v", left, right)
	}
}

type taxonomyCase struct {
	name string
	got  error
	want error
}

type taxonomyFixture struct {
	store   *Store
	data    []byte
	live    string
	expired string
	boot    string
}

func newTaxonomyFixture() taxonomyFixture {
	store := New()
	data := []byte("one\ntwo\n")
	expired := store.Put("e.txt", []byte("e\n"), nil)
	for range maxEntries {
		store.Put("f.txt", data, nil)
	}
	live := store.Put("f.txt", data, nil)
	boot, _, _ := strings.Cut(strings.TrimPrefix(live, "r"), ".")
	return taxonomyFixture{store: store, data: data, live: live, expired: expired, boot: boot}
}

func (f taxonomyFixture) get(ref string) error {
	_, err := f.store.Get(ref)
	return err
}

func (f taxonomyFixture) span(ref, path string, current []byte) error {
	return lookupSpan(f.store, ref, path, current)
}

func (f taxonomyFixture) gap(ref, path string) error {
	_, err := f.store.ResolveGap(ref, path, f.data, 0)
	return err
}

func (f taxonomyFixture) issued(seq uint64) string {
	return "r" + f.boot + "." + strconv.FormatUint(seq, 36)
}

func (f taxonomyFixture) foreign() string {
	return "r" + strings.Repeat("0", 32) + "." + strconv.FormatUint(refSequence(f.live), 36)
}

func lookupSpan(store *Store, ref, path string, current []byte) error {
	_, err := store.ResolveSpan(ref, path, current, Span{Start: 0, End: 1})
	return err
}

func removedPath() error {
	store := New()
	data := []byte("one\ntwo\n")
	ref := store.Put("f.txt", data, nil)
	store.Remove("f.txt", data)
	return lookupSpan(store, ref, "f.txt", data)
}

func movedPath() error {
	store := New()
	data := []byte("one\ntwo\n")
	ref := store.Put("a.txt", data, nil)
	store.Rename("a.txt", "b.txt", data, data, nil)
	return lookupSpan(store, ref, "a.txt", data)
}

func replacedDestination() error {
	store := New()
	data := []byte("one\ntwo\n")
	dest := store.Put("b.txt", []byte("dest\n"), nil)
	store.Put("a.txt", data, nil)
	store.Rename("a.txt", "b.txt", data, data, nil)
	return lookupSpan(store, dest, "b.txt", data)
}

func taxonomyCases() []taxonomyCase {
	f := newTaxonomyFixture()
	next := f.store.seq + 1
	stranger := New().Put("f.txt", f.data, nil)
	return []taxonomyCase{
		{"live get", f.get(f.live), nil},
		{"live span", f.span(f.live, "f.txt", f.data), nil},
		{"live gap", f.gap(f.live, "f.txt"), nil},
		{"evicted get", f.get(f.expired), ErrExpiredReference},
		{"evicted span", f.span(f.expired, "e.txt", f.data), ErrExpiredReference},
		{"evicted gap", f.gap(f.expired, "e.txt"), ErrExpiredReference},
		{"second sequence evicted", f.get(f.issued(refSequence(f.expired) + 1)), ErrExpiredReference},
		{"other boot get", f.get(f.foreign()), ErrUnknownReference},
		{"other boot span", f.span(f.foreign(), "f.txt", f.data), ErrUnknownReference},
		{"other boot gap", f.gap(f.foreign(), "f.txt"), ErrUnknownReference},
		{"another store's reference", f.span(stranger, "f.txt", f.data), ErrUnknownReference},
		{"never issued get", f.get(f.issued(next)), ErrUnknownReference},
		{"never issued span", f.span(f.issued(next), "f.txt", f.data), ErrUnknownReference},
		{"other path span", f.span(f.live, "g.txt", f.data), ErrBindingMismatch},
		{"other path gap", f.gap(f.live, "g.txt"), ErrBindingMismatch},
		{"external change", f.span(f.live, "f.txt", []byte("other\n")), ErrStaleReference},
		{"removed path", removedPath(), ErrPathChanged},
		{"moved path", movedPath(), ErrPathChanged},
		{"replaced destination", replacedDestination(), ErrStaleReference},
	}
}

func requireOnly(t *testing.T, name string, got, want error) {
	t.Helper()
	if want == nil {
		if got != nil {
			t.Fatalf("%s: unexpected error %v", name, got)
		}
		return
	}
	for _, sentinel := range []error{ErrUnknownReference, ErrExpiredReference, ErrBindingMismatch, ErrPathChanged, ErrStaleReference} {
		if errors.Is(got, sentinel) != errors.Is(want, sentinel) {
			t.Fatalf("%s: error %v, want only %v", name, got, want)
		}
	}
}

func TestReferenceTaxonomy(t *testing.T) {
	for _, item := range taxonomyCases() {
		requireOnly(t, item.name, item.got, item.want)
	}
}

func TestBindingMismatchNamesNoForeignData(t *testing.T) {
	f := newTaxonomyFixture()
	err := f.span(f.live, "g.txt", f.data)
	if !errors.Is(err, ErrBindingMismatch) {
		t.Fatalf("wrong path error %v", err)
	}
	if strings.Contains(err.Error(), "f.txt") || strings.Contains(err.Error(), f.live) {
		t.Fatalf("binding error leaks the bound entry: %q", err.Error())
	}
}

func TestMetadataRoundTrip(t *testing.T) {
	store := New()
	data := []byte("one\ntwo\n")
	ref := store.Put("f.txt", data, []Range{{Start: 2, End: 2}})
	entry, err := store.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	rebuilt := New()
	if restoreErr := rebuilt.Restore(entry.Meta(), data); restoreErr != nil {
		t.Fatal(restoreErr)
	}
	got, err := rebuilt.Get(ref)
	if err != nil {
		t.Fatal(err)
	}
	if got.Path != "f.txt" || got.Blob != Digest(data) || !rebuilt.Covers(ref, 2, 2) {
		t.Fatalf("restored entry %+v", got)
	}
}

func TestStoreLinesReportsTheReferenceLineCount(t *testing.T) {
	store := New()
	tests := []struct {
		name string
		data string
		want int
	}{
		{"empty", "", 0},
		{"one unterminated row", "x", 1},
		{"one terminated row", "x\n", 1},
		{"terminated rows", "a\nb\nc\n", 3},
		{"unterminated final row", "a\nb\nc", 3},
		{"blank rows", "\n\n", 2},
	}
	for _, test := range tests {
		t.Run("C08/"+test.name, func(t *testing.T) {
			ref := store.Put(test.name+".txt", []byte(test.data), nil)
			got, err := store.Lines(ref)
			if err != nil || got != test.want {
				t.Fatalf("Lines(%q) = %d, %v; want %d", test.data, got, err, test.want)
			}
		})
	}
	t.Run("C08/reference coordinates after a later edit", func(t *testing.T) {
		before := []byte("a\nb\nc\n")
		ref := store.Put("moved.txt", before, nil)
		store.Record("moved.txt", before, []byte("a\nb\nc\nd\ne\n"), []Splice{{Start: 3, End: 3, Inserted: 2}})
		got, err := store.Lines(ref)
		if err != nil || got != 3 {
			t.Fatalf("Lines after append = %d, %v; want 3", got, err)
		}
	})
	t.Run("C08/absent", requireLinesAbsent)
	t.Run("C08/nil store", requireLinesNilStore)
}

func requireLinesAbsent(t *testing.T) {
	f := newTaxonomyFixture()
	cases := []struct {
		name string
		ref  string
		want error
	}{
		{"expired", f.expired, ErrExpiredReference},
		{"other boot", f.foreign(), ErrUnknownReference},
		{"never issued", f.issued(f.store.seq + 1), ErrUnknownReference},
		{"malformed", "x", ErrUnknownReference},
	}
	for _, item := range cases {
		got, err := f.store.Lines(item.ref)
		if got != 0 {
			t.Fatalf("%s: count %d, want 0", item.name, got)
		}
		requireOnly(t, item.name, err, item.want)
	}
}

func requireLinesNilStore(t *testing.T) {
	var nilStore *Store
	got, err := nilStore.Lines("r0123456789abcdef0123456789abcdef.1")
	if got != 0 || !errors.Is(err, ErrUnknownReference) {
		t.Fatalf("nil store Lines = %d, %v; want 0, %v", got, err, ErrUnknownReference)
	}
}

func TestInvalidateRefusesEveryOlderReference(t *testing.T) {
	data := []byte("a\nb\n")
	t.Run("C13/old reference after the bytes return", func(t *testing.T) {
		store := New()
		old := store.Put("f.txt", data, []Range{{Start: 1, End: 2}})
		store.Invalidate("f.txt")
		if store.Unchanged(old, "f.txt", data) {
			t.Fatal("an invalidated path kept an eligible reference")
		}
		if _, err := store.ResolveSpan(old, "f.txt", data, Span{Start: 1, End: 1}); !errors.Is(err, ErrPathChanged) {
			t.Fatalf("ResolveSpan after Invalidate = %v, want %v", err, ErrPathChanged)
		}
		fresh := store.Put("f.txt", data, []Range{{Start: 1, End: 2}})
		if !store.Unchanged(fresh, "f.txt", data) || store.Unchanged(old, "f.txt", data) {
			t.Fatal("a read after Invalidate must give a fresh epoch and leave the old reference refused")
		}
	})
	t.Run("C13/other paths keep their references", func(t *testing.T) {
		store := New()
		other := store.Put("g.txt", data, nil)
		store.Invalidate("f.txt")
		if !store.Unchanged(other, "g.txt", data) {
			t.Fatal("Invalidate reached another path")
		}
	})
	t.Run("C13/absent path and nil store", func(t *testing.T) {
		store := New()
		store.Invalidate("never.txt")
		ref := store.Put("never.txt", data, nil)
		if !store.Unchanged(ref, "never.txt", data) {
			t.Fatal("Invalidate of an unseen path changed its first capture")
		}
		var none *Store
		none.Invalidate("f.txt")
	})
}

func TestStoreShownReturnsACopyOfTheRanges(t *testing.T) {
	t.Run("C15/ranges of the reference", requireShownCopy)
	t.Run("C15/no range shown", requireNothingShown)
	t.Run("C15/absent references", requireShownAbsent)
}

func requireShownCopy(t *testing.T) {
	store := New()
	ref := store.Put("f.txt", []byte("a\nb\nc\nd\n"), []Range{{Start: 1, End: 1}, {Start: 3, End: 4}})
	got, err := store.Shown(ref)
	want := []Range{{Start: 1, End: 1}, {Start: 3, End: 4}}
	if err != nil || !slices.Equal(got, want) {
		t.Fatalf("Shown = %v, %v; want %v", got, err, want)
	}
	got[0].End = 9
	if again, _ := store.Shown(ref); !slices.Equal(again, want) {
		t.Fatalf("a caller write reached the store: %v", again)
	}
}

func requireNothingShown(t *testing.T) {
	store := New()
	got, err := store.Shown(store.Put("f.txt", []byte("a\n"), nil))
	if err != nil || len(got) != 0 {
		t.Fatalf("Shown = %v, %v; want no range", got, err)
	}
}

func requireShownAbsent(t *testing.T) {
	store := New()
	first := store.Put("f.txt", []byte("body"), nil)
	for range maxEntries {
		store.Put("f.txt", []byte("body"), nil)
	}
	if _, err := store.Shown(first); !errors.Is(err, ErrExpiredReference) {
		t.Fatalf("evicted reference error %v", err)
	}
	if _, err := store.Shown(New().Put("f.txt", []byte("body"), nil)); !errors.Is(err, ErrUnknownReference) {
		t.Fatalf("foreign reference error %v", err)
	}
	var none *Store
	if _, err := none.Shown(first); !errors.Is(err, ErrUnknownReference) {
		t.Fatalf("nil store error %v", err)
	}
}
