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
	m, err := readManifest(filepath.Join(last, manifestFile))
	if err != nil {
		b.Fatal(err)
	}
	rejected := 0
	for _, rec := range m.Entries {
		if !rec.Indexed {
			rejected++
		}
	}
	info, err := os.Stat(filepath.Join(last, trigramFile))
	if err != nil {
		b.Fatal(err)
	}
	b.ReportMetric(float64(info.Size()), "index_bytes")
	b.ReportMetric(float64(rejected), "rejected_files")
}

func BenchmarkBuildIndexWorkloadA(b *testing.B) {
	requireChangeTimes(b)
	ws := b.TempDir()
	writeWorkload(b, ws, 2000, 10000, 1)
	benchBuildIndex(b, ws)
}

func BenchmarkBuildIndexWorkloadB(b *testing.B) {
	requireChangeTimes(b)
	benchBuildIndex(b, corpusDir(b))
}
