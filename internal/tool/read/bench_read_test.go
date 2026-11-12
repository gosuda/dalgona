package read

import (
	"encoding/json/jsontext"
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

const benchLetters = "abcdefghijklmnopqrstuvwxyz"

func benchReadEnv(b *testing.B) tool.Env {
	b.Helper()
	local, err := runner.NewLocal(b.TempDir())
	if err != nil {
		b.Fatal(err)
	}
	return tool.Env{Runner: local, Cwd: b.TempDir(), Home: b.TempDir(), Snapshots: snapshot.New()}
}

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

func BenchmarkReadToolWindowMiddle(b *testing.B) {
	env := benchReadEnv(b)
	var big strings.Builder
	rng := rand.New(rand.NewPCG(1, 2))
	for range 100000 {
		for range 39 {
			big.WriteByte(benchLetters[rng.IntN(len(benchLetters))])
		}
		big.WriteByte('\n')
	}
	if err := os.WriteFile(filepath.Join(env.Cwd, "big.txt"), []byte(big.String()), 0o644); err != nil {
		b.Fatal(err)
	}
	args := jsontext.Value(`{"path":"big.txt","offset":50000,"limit":200}`)
	if _, err := (Tool{}).Run(b.Context(), tool.Call{Args: args}, env); err != nil {
		b.Fatal(err)
	}
	benchSamples(b, func() {
		if _, err := (Tool{}).Run(b.Context(), tool.Call{Args: args}, env); err != nil {
			b.Fatal(err)
		}
	})
}

func BenchmarkReadToolSmallFiles(b *testing.B) {
	env := benchReadEnv(b)
	rng := rand.New(rand.NewPCG(1, 2))
	paths := make([]string, 1500)
	for i := range paths {
		paths[i] = filepath.Join("a", "b", "c", "d", "e", fmt.Sprintf("f%04d.txt", i))
		if err := os.MkdirAll(filepath.Join(env.Cwd, filepath.Dir(paths[i])), 0o755); err != nil {
			b.Fatal(err)
		}
		body := make([]byte, 2048)
		for j := range body {
			body[j] = benchLetters[rng.IntN(len(benchLetters))]
		}
		if err := os.WriteFile(filepath.Join(env.Cwd, paths[i]), body, 0o644); err != nil {
			b.Fatal(err)
		}
	}
	seeded := make([]string, 200)
	for i := range seeded {
		seeded[i] = paths[rng.IntN(len(paths))]
	}
	read := func(rel string) {
		args := jsontext.Value(fmt.Sprintf(`{"path":%q}`, filepath.ToSlash(rel)))
		if _, err := (Tool{}).Run(b.Context(), tool.Call{Args: args}, env); err != nil {
			b.Fatal(err)
		}
	}
	for _, rel := range seeded {
		read(rel)
	}
	next := 0
	benchSamples(b, func() {
		read(seeded[next%len(seeded)])
		next++
	})
}
