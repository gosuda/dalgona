package snapshot

import (
	"bytes"
	"math/rand/v2"
	"slices"
	"testing"
	"time"
)

func benchSamples(b *testing.B, op func()) {
	b.Helper()
	b.ReportAllocs()
	samples := make([]time.Duration, 0, b.N)
	for range b.N {
		start := time.Now()
		op()
		samples = append(samples, time.Since(start))
	}
	b.ReportMetric(float64(percentile(samples, 0.50).Nanoseconds()), "p50_ns")
	b.ReportMetric(float64(percentile(samples, 0.95).Nanoseconds()), "p95_ns")
	b.ReportMetric(float64(percentile(samples, 0.99).Nanoseconds()), "p99_ns")
	b.ReportMetric(float64(slices.Max(samples).Nanoseconds()), "max_ns")
}

func percentile(samples []time.Duration, p float64) time.Duration {
	sorted := slices.Clone(samples)
	slices.Sort(sorted)
	return sorted[min(len(sorted)-1, int(p*float64(len(sorted))))]
}

func BenchmarkSnapshotCaptureHashing(b *testing.B) {
	const size = 16 << 20
	rng := rand.New(rand.NewPCG(1, 2))
	data := make([]byte, 0, size)
	for len(data) < size {
		data = append(data, []byte("line of text for the capture benchmark\n")...)
		data = append(data, "abcdefghijklmnopqrstuvwxyz"[rng.IntN(26)])
	}
	data = bytes.Clone(data[:size])
	b.Run("capture", func(b *testing.B) {
		store := New()
		b.SetBytes(size)
		benchSamples(b, func() {
			if _, ok := store.Capture("big.txt", data, nil); !ok {
				b.Fatal("capture refused")
			}
		})
	})
	b.Run("tag", func(b *testing.B) {
		b.SetBytes(size)
		benchSamples(b, func() { _ = Tag("whole", data) })
	})
	b.Run("version_tag", func(b *testing.B) {
		b.SetBytes(size)
		benchSamples(b, func() { _ = VersionTag(data) })
	})
}
