package search

import (
	"fmt"
	"math/rand/v2"
	"path/filepath"
	"slices"
	"strings"
	"testing"
)

func benchManifest(count int) manifest {
	rng := rand.New(rand.NewPCG(1, 2))
	entries := make([]manifestEntry, count)
	for i := range entries {
		depth := 3 + rng.IntN(6)
		parts := make([]string, 0, depth+1)
		for range depth {
			parts = append(parts, fmt.Sprintf("p%02d", rng.IntN(50)))
		}
		parts = append(parts, fmt.Sprintf("f%07d.go", i))
		entries[i] = manifestEntry{
			Path:    strings.Join(parts, "/"),
			Size:    int64(rng.IntN(1 << 20)),
			ModNS:   1700000000000000000 + int64(i),
			CtimeNS: 1700000000000000000 + int64(i) + 1,
			Indexed: i%7 != 0,
		}
	}
	slices.SortFunc(entries, func(a, b manifestEntry) int { return strings.Compare(a.Path, b.Path) })
	return manifest{Start: 1700000100000000000, TrigramSize: 1 << 20, Entries: entries}
}

func BenchmarkManifestLoad(b *testing.B) {
	for _, count := range []int{2000, 200000} {
		b.Run(fmt.Sprintf("entries%d", count), func(b *testing.B) {
			path := filepath.Join(b.TempDir(), "manifest")
			if err := writeManifest(path, benchManifest(count)); err != nil {
				b.Fatal(err)
			}
			if _, err := readManifest(path); err != nil {
				b.Fatal(err)
			}
			benchSamples(b, nil, func() {
				if _, err := readManifest(path); err != nil {
					b.Fatal(err)
				}
			})
		})
	}
}
