package search

import (
	"fmt"
	"math/rand/v2"
	"testing"
)

func BenchmarkRenderGrepManyFiles(b *testing.B) {
	rng := rand.New(rand.NewPCG(1, 2))
	files := make([]grepFile, 50000)
	for i := range files {
		data := textBytes(rng, 2048)
		files[i] = grepFile{
			entry:  fileEntry{display: fmt.Sprintf("r%05d.txt", i)},
			data:   data,
			starts: splitStarts(data),
		}
	}
	kept := make([]hit, 0, 1000)
	for k := range 1000 {
		kept = append(kept, hit{file: k * 50, line: 1 + k%20})
	}
	rendered := renderGrep(files, kept, len(kept), "needle")
	if rendered.Text == "" {
		b.Fatal("render produced no rows")
	}
	benchSamples(b, nil, func() {
		_ = renderGrep(files, kept, len(kept), "needle")
	})
}
