package search

import (
	"bytes"
	"os"
	"path/filepath"
	"slices"
	"testing"
)

func TestWriteWorkloadShape(t *testing.T) {
	root := t.TempDir()
	paths := writeWorkload(t, root, 2000, 10000)
	if len(paths) != 2000 {
		t.Fatalf("paths = %d, want 2000", len(paths))
	}
	if !slices.IsSorted(paths) {
		t.Fatal("paths are not sorted")
	}
	var total int64
	for _, rel := range paths {
		info, err := os.Stat(filepath.Join(root, filepath.FromSlash(rel)))
		if err != nil {
			t.Fatal(err)
		}
		if info.Size() != 10000 {
			t.Fatalf("%s is %d bytes, want 10000", rel, info.Size())
		}
		total += info.Size()
	}
	if total != 20000000 {
		t.Fatalf("total = %d bytes, want 20000000", total)
	}
}

func TestWriteWorkloadIsSeeded(t *testing.T) {
	first, second := t.TempDir(), t.TempDir()
	paths := writeWorkload(t, first, 3, 64)
	again := writeWorkload(t, second, 3, 64)
	if !slices.Equal(paths, again) {
		t.Fatalf("paths differ: %v and %v", paths, again)
	}
	for _, rel := range paths {
		a, err := os.ReadFile(filepath.Join(first, filepath.FromSlash(rel)))
		if err != nil {
			t.Fatal(err)
		}
		b, err := os.ReadFile(filepath.Join(second, filepath.FromSlash(rel)))
		if err != nil {
			t.Fatal(err)
		}
		if !bytes.Equal(a, b) {
			t.Fatalf("%s differs between runs with the same seed", rel)
		}
	}
}
