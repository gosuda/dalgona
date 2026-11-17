package search

import (
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"regexp"
	"testing"
)

func BenchmarkScanFileMatchAll(b *testing.B) {
	root := b.TempDir()
	names := make([]string, 150)
	for i := range names {
		names[i] = fmt.Sprintf("s%03d.txt", i)
	}
	entries := writeTextFiles(b, root, names, 100<<10, rand.New(rand.NewPCG(1, 2)))
	matcher := regexp.MustCompile("e")
	b.SetBytes(int64(len(entries)) * (100 << 10))
	for _, entry := range entries {
		_, _ = scanFile(entry, matcher)
	}
	benchSamples(b, nil, func() {
		for _, entry := range entries {
			_, _ = scanFile(entry, matcher)
		}
	})
}

func BenchmarkReadCappedLarge(b *testing.B) {
	path := filepath.Join(b.TempDir(), "large.txt")
	if err := os.WriteFile(path, textBytes(rand.New(rand.NewPCG(1, 2)), 16<<20), 0o644); err != nil {
		b.Fatal(err)
	}
	b.SetBytes(16 << 20)
	if _, err := readCapped(path); err != nil {
		b.Fatal(err)
	}
	benchSamples(b, nil, func() {
		if _, err := readCapped(path); err != nil {
			b.Fatal(err)
		}
	})
}
