package snapshot

import (
	"errors"
	"testing"
)

func TestReferenceGrammar(t *testing.T) {
	valid := "r0123456789abcdef0123456789abcdef.1a"
	if !ValidRef(valid) {
		t.Fatal("valid reference rejected")
	}
	invalid := []string{"", "r", "rabc.1", "r0123456789abcdef0123456789abcdef.0", "r0123456789abcdef0123456789abcdef.01", "r0123456789abcdef0123456789ABCDEF.1", "r0123456789abcdef0123456789abcdef.zzzzzzzzzzzzz"}
	for _, ref := range invalid {
		if ValidRef(ref) {
			t.Fatalf("accepted %q", ref)
		}
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
	for range maxEntries {
		ref := store.Put("f.txt", []byte("body"), nil)
		if seen[ref] {
			t.Fatalf("reference reused: %s", ref)
		}
		seen[ref] = true
	}
	if _, err := store.Get(first); !errors.Is(err, ErrUnknownReference) {
		t.Fatalf("old reference error %v", err)
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
