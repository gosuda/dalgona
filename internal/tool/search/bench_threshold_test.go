package search

import (
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"testing"
	"time"
)

const sweepFileBytes = 16 << 20

func writeUncoveredText(b *testing.B, dir string, total int64) {
	b.Helper()
	rng := rand.New(rand.NewPCG(1, 2))
	for i := int64(0); i*sweepFileBytes < total; i++ {
		size := min(sweepFileBytes, total-i*sweepFileBytes)
		path := filepath.Join(dir, "sweep", fmt.Sprintf("u%05d.txt", i))
		if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
			b.Fatal(err)
		}
		if err := os.WriteFile(path, textBytes(rng, int(size)), 0o600); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkThresholdSweep(b *testing.B) {
	requireChangeTimes(b)
	corpus := corpusDir(b)
	start := time.Now().Add(time.Hour)
	dir := benchBuiltGeneration(b, corpus, start)
	clock := clockAt(start)
	for size := int64(1 << 20); size <= 4<<30; size <<= 1 {
		writeUncoveredText(b, corpus, size)
		b.Run(fmt.Sprintf("uncovered=%d", size), func(b *testing.B) {
			b.Run("indexed", func(b *testing.B) { benchGrep(b, corpus, dir, clock, "needle-7f3a") })
			b.Run("scan", func(b *testing.B) { benchGrep(b, corpus, "", freshness{}, "needle-7f3a") })
		})
		if err := os.RemoveAll(filepath.Join(corpus, "sweep")); err != nil {
			b.Fatal(err)
		}
	}
}
