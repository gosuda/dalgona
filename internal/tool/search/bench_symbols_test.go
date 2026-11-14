package search

import (
	"bytes"
	"fmt"
	"math/rand/v2"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"
)

const (
	narrowingNeedle = "NeedleRare"
	notParsedTag    = "[Not parsed: "
)

func writeNarrowingCorpus(tb testing.TB, root string, files, dirs int) {
	tb.Helper()
	for d := range dirs {
		if err := os.MkdirAll(filepath.Join(root, fmt.Sprintf("d%04d", d)), 0o750); err != nil {
			tb.Fatal(err)
		}
	}
	for i := range files {
		head := fmt.Sprintf("package p\n\nfunc F%06d() int { return %d }\n", i, i)
		if i < 3 {
			head = "package p\n\nfunc " + narrowingNeedle + "() int { return 1 }\n"
		}
		pad := 512 - len(head) - len("// \n")
		body := head + "// " + strings.Repeat("abcdefgh", pad/8+1)[:pad] + "\n"
		path := filepath.Join(root, fmt.Sprintf("d%04d", i%dirs), fmt.Sprintf("f%06d.go", i))
		if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
			tb.Fatal(err)
		}
	}
}

func writeGoCorpus(tb testing.TB, root string, files, bytesEach int) {
	tb.Helper()
	rng := rand.New(rand.NewPCG(1, 2))
	for i := range files {
		var buf bytes.Buffer
		buf.WriteString("package p\n\n")
		for n := 0; ; n++ {
			fn := fmt.Sprintf("func F%03d_%05d(x int) int {\n\treturn x + %d\n}\n\n", i, n, rng.IntN(1<<20))
			if buf.Len()+len(fn) > bytesEach {
				break
			}
			buf.WriteString(fn)
		}
		path := filepath.Join(root, fmt.Sprintf("f%03d.go", i))
		if err := os.WriteFile(path, buf.Bytes(), 0o600); err != nil {
			tb.Fatal(err)
		}
	}
}

func narrowingParsedFiles(tb testing.TB, root string, a args) int {
	tb.Helper()
	entries, err := walkScope(root, root)
	if err != nil {
		tb.Fatal(err)
	}
	matcher, err := namePattern(a)
	if err != nil {
		tb.Fatal(err)
	}
	var tally callTally
	return len(dispatchSymbols(entries, matcher, time.Now(), time.Now, &tally))
}

func BenchmarkSymbolsWalkNarrowing(b *testing.B) {
	root := b.TempDir()
	writeNarrowingCorpus(b, root, 200_000, 2_000)
	a := args{Mode: "symbols", Pattern: narrowingNeedle, Literal: true, Path: ".", Limit: 1000}
	b.ReportMetric(float64(narrowingParsedFiles(b, root, a)), "parsed_files")
	if _, err := execute(root, a, "", freshness{}); err != nil {
		b.Fatal(err)
	}
	benchSamples(b, nil, func() {
		if _, err := execute(root, a, "", freshness{}); err != nil {
			b.Fatal(err)
		}
	})
}

func BenchmarkSymbolsWorkers(b *testing.B) {
	root := b.TempDir()
	writeGoCorpus(b, root, 64, 64<<10)
	a := args{Mode: "symbols", Path: ".", Limit: 200}
	for _, n := range []int{1, 4, 8, runtime.GOMAXPROCS(0)} {
		b.Run(fmt.Sprintf("workers=%d", n), func(b *testing.B) {
			prev := parseWorkerCount
			parseWorkerCount = n
			b.Cleanup(func() { parseWorkerCount = prev })
			if _, err := execute(root, a, "", freshness{}); err != nil {
				b.Fatal(err)
			}
			benchSamples(b, nil, func() {
				if _, err := execute(root, a, "", freshness{}); err != nil {
					b.Fatal(err)
				}
			})
		})
	}
}

func BenchmarkSymbolsChildPeakRSS(b *testing.B) {
	root := b.TempDir()
	writeGoCorpus(b, root, 64, 256<<10)
	const argv = `{"mode":"symbols","path":"."}`
	exe := testExecutable(b)
	var peak, notParsed int64
	for range b.N {
		cmd := exec.Command(exe, "__search", argv)
		cmd.Dir = root
		out, err := cmd.Output()
		if err != nil {
			b.Fatalf("child: %v\n%s", err, out)
		}
		peak = max(peak, peakRSSKiB(cmd))
		notParsed, err = notParsedCount(out)
		if err != nil {
			b.Fatal(err)
		}
	}
	b.ReportMetric(float64(peak), "child_peak_rss_kib")
	b.ReportMetric(float64(notParsed), "not_parsed_files")
}

func notParsedCount(out []byte) (int64, error) {
	_, rest, found := bytes.Cut(out, []byte(notParsedTag))
	if !found {
		return 0, nil
	}
	digits, _, _ := bytes.Cut(rest, []byte(" "))
	return strconv.ParseInt(string(digits), 10, 64)
}
