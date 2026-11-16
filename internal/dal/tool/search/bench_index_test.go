package search

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

func benchBuildIndex(b *testing.B, ws string) {
	b.Helper()
	root := b.TempDir()
	var last string
	benchSamples(b, nil, func() {
		_, dir, err := createGeneration(root, time.Now())
		if err != nil {
			b.Fatal(err)
		}
		if err := buildIndex(ws, dir, time.Now(), 1<<30); err != nil {
			b.Fatal(err)
		}
		last = dir
	})
	rejected := benchRejectedFiles(b, last)
	info, err := os.Stat(filepath.Join(last, trigramFile))
	if err != nil {
		b.Fatal(err)
	}
	b.ReportMetric(float64(info.Size()), "index_bytes")
	b.ReportMetric(float64(rejected), "rejected_files")
}

func benchRejectedFiles(b *testing.B, dir string) int {
	b.Helper()
	m, err := readManifest(filepath.Join(dir, manifestFile))
	if err != nil {
		b.Fatal(err)
	}
	rejected := 0
	for _, rec := range m.Entries {
		if !rec.Indexed {
			rejected++
		}
	}
	return rejected
}

func BenchmarkBuildIndexWorkloadA(b *testing.B) {
	requireChangeTimes(b)
	ws := b.TempDir()
	writeWorkload(b, ws, 2000, 10000)
	benchBuildIndex(b, ws)
}

func BenchmarkBuildIndexWorkloadB(b *testing.B) {
	requireChangeTimes(b)
	benchBuildIndex(b, corpusDir(b))
}

func benchGrep(b *testing.B, ws, dir string, clock freshness, pattern string) {
	b.Helper()
	query := args{Mode: "grep", Pattern: pattern, Literal: true, Limit: 200}
	benchSamples(b, nil, func() {
		if _, err := execute(ws, query, dir, clock); err != nil {
			b.Fatal(err)
		}
	})
}

func benchBuiltGeneration(b *testing.B, ws string, start time.Time) string {
	b.Helper()
	_, dir, err := createGeneration(b.TempDir(), start)
	if err != nil {
		b.Fatal(err)
	}
	if err := buildIndex(ws, dir, start, 1<<30); err != nil {
		b.Fatal(err)
	}
	return dir
}

func BenchmarkGrepRareLiteral(b *testing.B) {
	requireChangeTimes(b)
	ws := b.TempDir()
	names := writeWorkload(b, ws, 2000, 10000)
	if err := os.WriteFile(filepath.Join(ws, filepath.FromSlash(names[1000])), []byte("needle-7f3a\n"), 0o600); err != nil {
		b.Fatal(err)
	}
	start := time.Now().Add(time.Hour)
	dir := benchBuiltGeneration(b, ws, start)
	rejected := benchRejectedFiles(b, dir)
	b.Run("scan", func(b *testing.B) {
		benchGrep(b, ws, "", freshness{}, "needle-7f3a")
		b.ReportMetric(float64(rejected), "rejected_files")
	})
	b.Run("indexed", func(b *testing.B) {
		benchGrep(b, ws, dir, clockAt(start), "needle-7f3a")
		b.ReportMetric(float64(rejected), "rejected_files")
	})
}

func BenchmarkGrepRareLiteralWorkloadB(b *testing.B) {
	requireChangeTimes(b)
	ws := corpusDir(b)
	start := time.Now().Add(time.Hour)
	dir := benchBuiltGeneration(b, ws, start)
	rejected := benchRejectedFiles(b, dir)
	b.Run("scan", func(b *testing.B) {
		benchGrep(b, ws, "", freshness{}, "needle-7f3a")
		b.ReportMetric(float64(rejected), "rejected_files")
	})
	b.Run("indexed", func(b *testing.B) {
		benchGrep(b, ws, dir, clockAt(start), "needle-7f3a")
		b.ReportMetric(float64(rejected), "rejected_files")
	})
}

func BenchmarkGrepMatchAll(b *testing.B) {
	requireChangeTimes(b)
	ws := b.TempDir()
	writeWorkload(b, ws, 2000, 10000)
	start := time.Now().Add(time.Hour)
	dir := benchBuiltGeneration(b, ws, start)
	b.Run("scan", func(b *testing.B) { benchGrep(b, ws, "", freshness{}, "e") })
	b.Run("indexed", func(b *testing.B) { benchGrep(b, ws, dir, clockAt(start), "e") })
}

func BenchmarkWalkStatPassPlanReads(b *testing.B) {
	ws := b.TempDir()
	writeWorkload(b, ws, 2000, 10000)
	entries, err := walkScope(ws, ws)
	if err != nil {
		b.Fatal(err)
	}
	benchSamples(b, nil, func() { planReads(entries, nil) })
}
