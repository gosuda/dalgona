package search

import (
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"
)

const (
	workloadBytes   = "abcdefghijklmnopqrstuvwxyz \n"
	workloadLetters = "abcdefghijklmnopqrstuvwxyz "
)

func writeWorkload(tb testing.TB, root string, files, bytesEach int, seed uint64) []string {
	tb.Helper()
	rng := rand.New(rand.NewPCG(seed, 2))
	paths := make([]string, 0, files)
	body := make([]byte, bytesEach)
	for i := range files {
		rel := fmt.Sprintf("d%03d/f%05d.txt", i%200, i)
		for j := range body {
			body[j] = workloadBytes[rng.IntN(len(workloadBytes))]
		}
		path := filepath.Join(root, filepath.FromSlash(rel))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			tb.Fatal(err)
		}
		if err := os.WriteFile(path, body, 0o644); err != nil {
			tb.Fatal(err)
		}
		paths = append(paths, rel)
	}
	slices.Sort(paths)
	return paths
}

func textBytes(rng *rand.Rand, size int) []byte {
	out := make([]byte, 0, size+101)
	for len(out) < size {
		for range 40 + rng.IntN(61) {
			out = append(out, workloadLetters[rng.IntN(len(workloadLetters))])
		}
		out = append(out, '\n')
	}
	return out[:size]
}

func writeTextFiles(tb testing.TB, root string, names []string, size int, rng *rand.Rand) []fileEntry {
	tb.Helper()
	entries := make([]fileEntry, 0, len(names))
	for _, name := range names {
		path := filepath.Join(root, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			tb.Fatal(err)
		}
		if err := os.WriteFile(path, textBytes(rng, size), 0o644); err != nil {
			tb.Fatal(err)
		}
		entries = append(entries, fileEntry{absolute: path, display: name, searchRel: name})
	}
	return entries
}

func corpusDir(b *testing.B) string {
	b.Helper()
	dir := os.Getenv("DALGONA_SEARCH_BENCH_CORPUS")
	if dir == "" {
		b.Skip("set DALGONA_SEARCH_BENCH_CORPUS to a directory for workload B")
	}
	marker := filepath.Join(dir, ".corpus-ready")
	if _, err := os.Stat(marker); err != nil {
		writeWorkload(b, dir, 200000, 20000, 1)
		if err := os.WriteFile(marker, nil, 0o644); err != nil {
			b.Fatal(err)
		}
	}
	return dir
}

func benchSamples(b *testing.B, setup, op func()) float64 {
	b.Helper()
	b.ReportAllocs()
	samples := make([]time.Duration, 0, b.N)
	for range b.N {
		if setup != nil {
			b.StopTimer()
			setup()
			b.StartTimer()
		}
		start := time.Now()
		op()
		samples = append(samples, time.Since(start))
	}
	slices.Sort(samples)
	at := func(p float64) float64 {
		return float64(samples[min(len(samples)-1, int(p*float64(len(samples))))].Nanoseconds())
	}
	b.ReportMetric(at(0.50), "p50_ns")
	b.ReportMetric(at(0.95), "p95_ns")
	b.ReportMetric(at(0.99), "p99_ns")
	b.ReportMetric(float64(samples[len(samples)-1].Nanoseconds()), "max_ns")
	return at(0.50)
}
