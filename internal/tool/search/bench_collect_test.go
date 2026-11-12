package search

import (
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"testing"
)

func BenchmarkCollectMatchesRetention(b *testing.B) {
	root := b.TempDir()
	rng := rand.New(rand.NewPCG(1, 2))
	entries := make([]fileEntry, 0, 4000)
	for i := range 4000 {
		name := fmt.Sprintf("c%04d.txt", i)
		body := textBytes(rng, 64<<10)
		if i%2 == 0 {
			copy(body, "needle here\n")
		}
		path := filepath.Join(root, name)
		if err := os.WriteFile(path, body, 0o644); err != nil {
			b.Fatal(err)
		}
		entries = append(entries, fileEntry{absolute: path, display: name, searchRel: name})
	}
	matcher := regexp.MustCompile("needle")
	_, _, _ = collectMatches(entries, matcher, 200)
	benchSamples(b, func() { runtime.GC() }, func() {
		_, _, _ = collectMatches(entries, matcher, 200)
	})
	runtime.GC()
	scanned, kept, total := collectMatches(entries, matcher, 200)
	var stats runtime.MemStats
	runtime.ReadMemStats(&stats)
	b.ReportMetric(float64(stats.HeapInuse), "heap_inuse_bytes")
	runtime.KeepAlive(scanned)
	runtime.KeepAlive(kept)
	runtime.KeepAlive(total)
}
