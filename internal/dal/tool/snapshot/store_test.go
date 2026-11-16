package snapshot

import (
	"errors"
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
