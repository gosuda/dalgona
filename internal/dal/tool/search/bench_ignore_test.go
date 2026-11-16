package search

import (
	"fmt"
	"math/rand/v2"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func ignoreBenchPaths(root string, count int) []string {
	rng := rand.New(rand.NewPCG(1, 2))
	out := make([]string, count)
	for i := range out {
		depth := 3 + rng.IntN(6)
		parts := make([]string, 0, depth+1)
		for range depth {
			if rng.IntN(2) == 0 {
				parts = append(parts, fmt.Sprintf("skip%02d", rng.IntN(60)))
			} else {
				parts = append(parts, fmt.Sprintf("dir%03d", rng.IntN(500)))
			}
		}
		parts = append(parts, fmt.Sprintf("f%06d.go", i))
		out[i] = filepath.Join(root, filepath.Join(parts...))
	}
	return out
}

func BenchmarkIgnoredFileDirOnlyRules(b *testing.B) {
	root := b.TempDir()
	var gitignore strings.Builder
	for i := range 40 {
		fmt.Fprintf(&gitignore, "skip%02d/\n", i)
	}
	for i := range 10 {
		fmt.Fprintf(&gitignore, "plain%02d\n", i)
	}
	if err := os.WriteFile(filepath.Join(root, ".gitignore"), []byte(gitignore.String()), 0o644); err != nil {
		b.Fatal(err)
	}
	stack := loadIgnore(root)
	paths := ignoreBenchPaths(root, 200000)
	p50 := benchSamples(b, nil, func() {
		for _, p := range paths {
			_ = stack.ignored(p, false)
		}
	})
	b.ReportMetric(p50/float64(len(paths)), "ns/path")
}

func ignoreStackOf(b *testing.B, root string, rules int) ignoreStack {
	b.Helper()
	var stack ignoreStack
	for d := range rules / 5 {
		dir := filepath.Join(root, fmt.Sprintf("sub%04d", d))
		if err := os.MkdirAll(dir, 0o755); err != nil {
			b.Fatal(err)
		}
		var gitignore strings.Builder
		for r := range 5 {
			fmt.Fprintf(&gitignore, "rule%04d_%d.tmp\n", d, r)
		}
		if err := os.WriteFile(filepath.Join(dir, ".gitignore"), []byte(gitignore.String()), 0o644); err != nil {
			b.Fatal(err)
		}
		stack.loadDir(root, dir)
	}
	return stack
}

func BenchmarkIgnoredRuleStack(b *testing.B) {
	for _, rules := range []int{10, 100, 1000, 10000} {
		b.Run(fmt.Sprintf("R%d", rules), func(b *testing.B) {
			root := b.TempDir()
			stack := ignoreStackOf(b, root, rules)
			last := filepath.Join(root, fmt.Sprintf("sub%04d", rules/5-1))
			paths := make([]string, 10000)
			for i := range paths {
				paths[i] = filepath.Join(last, fmt.Sprintf("f%05d.txt", i))
			}
			p50 := benchSamples(b, nil, func() {
				for _, p := range paths {
					_ = stack.ignored(p, false)
				}
			})
			b.ReportMetric(p50/float64(len(paths)), "ns/path")
		})
	}
}
