package search

import (
	"cmp"
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"testing"
)

func writeDeepTree(tb testing.TB, root string, files, dirs int) {
	tb.Helper()
	for i := range files {
		d := i % dirs
		depth := 6 + d%3
		parts := make([]string, 0, depth+1)
		q := d
		for range depth {
			parts = append(parts, "g"+strconv.Itoa(q%10))
			q /= 10
		}
		parts = append(parts, fmt.Sprintf("f%05d.txt", i))
		path := filepath.Join(root, filepath.Join(parts...))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			tb.Fatal(err)
		}
		if err := os.WriteFile(path, []byte("x\n"), 0o644); err != nil {
			tb.Fatal(err)
		}
	}
}

func BenchmarkWalkScopeFind(b *testing.B) {
	root := b.TempDir()
	writeDeepTree(b, root, 20000, 2000)
	parsed := args{Mode: "find", Pattern: "no-such-name", Path: ".", Limit: 1}
	if _, err := execute(root, parsed, "", freshness{}); err != nil {
		b.Fatal(err)
	}
	benchSamples(b, nil, func() {
		if _, err := execute(root, parsed, "", freshness{}); err != nil {
			b.Fatal(err)
		}
	})
}

func shuffledEntries(count int) []fileEntry {
	rng := rand.New(rand.NewPCG(1, 2))
	entries := make([]fileEntry, count)
	for i, n := range rng.Perm(count) {
		name := fmt.Sprintf("p%06d/f.txt", n)
		entries[i] = fileEntry{absolute: name, display: name, searchRel: name}
	}
	return entries
}

func BenchmarkWalkSortOnly(b *testing.B) {
	entries := shuffledEntries(200000)
	work := make([]fileEntry, len(entries))
	copy(work, entries)
	slices.SortFunc(work, func(x, y fileEntry) int { return cmp.Compare(x.display, y.display) })
	benchSamples(b, func() { copy(work, entries) }, func() {
		slices.SortFunc(work, func(x, y fileEntry) int { return cmp.Compare(x.display, y.display) })
	})
}

func benchWalk(b *testing.B, root string) {
	b.Helper()
	if _, err := walkScope(root, root); err != nil {
		b.Fatal(err)
	}
	benchSamples(b, nil, func() {
		if _, err := walkScope(root, root); err != nil {
			b.Fatal(err)
		}
	})
}

func BenchmarkWalkStatPass(b *testing.B) {
	b.Run("walk/A", func(b *testing.B) {
		root := b.TempDir()
		writeWorkload(b, root, 2000, 10000)
		benchWalk(b, root)
	})
	b.Run("walk/B", func(b *testing.B) {
		benchWalk(b, corpusDir(b))
	})
}
