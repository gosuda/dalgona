package search

import (
	"fmt"
	"math/rand/v2"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/tool/snapshot"
)

func BenchmarkSearchOutcomeCodec(b *testing.B) {
	rows := make([]string, 200)
	for i := range rows {
		rows[i] = fmt.Sprintf("file%03d.txt:%d: line text %d", i%50, i, i)
	}
	shown := make([]snapshot.Range, 200)
	for i := range shown {
		shown[i] = snapshot.Range{Start: i*5000 + 1, End: i*5000 + 2}
	}
	result := outcome{
		Text:     strings.Join(rows, "\n"),
		Captures: []capture{{Path: "big.txt", Data: textBytes(rand.New(rand.NewPCG(1, 2)), 16<<20), Shown: shown}},
	}
	encoded := encodeOutcome(result)
	b.Run("encode", func(b *testing.B) {
		b.SetBytes(int64(len(encoded)))
		benchSamples(b, nil, func() { _ = encodeOutcome(result) })
	})
	b.Run("decode", func(b *testing.B) {
		b.SetBytes(int64(len(encoded)))
		if _, err := decodeOutcome(encoded); err != nil {
			b.Fatal(err)
		}
		benchSamples(b, nil, func() {
			if _, err := decodeOutcome(encoded); err != nil {
				b.Fatal(err)
			}
		})
	})
}
