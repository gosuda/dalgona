//go:build gui

package composer

import (
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const keywordMatchLimit = 64 << 10

type KeywordSpan struct {
	Word  string
	Bytes inputtext.Range
	Runes inputtext.Range
	Paint inputtext.Range
}

func KeywordSpans(text string, matches []rpc.KeywordMatch) ([]KeywordSpan, bool) {
	if !utf8.ValidString(text) {
		return nil, false
	}
	spans := make([]KeywordSpan, 0, len(matches))
	runes := runeCounter{text: text}
	previous := 0
	for _, m := range matches {
		r := inputtext.Range{Start: m.ByteRange.Start, End: m.ByteRange.End}
		if r.Start < previous || r.End <= r.Start {
			return nil, false
		}
		paint, err := inputtext.PaintRange(text, r)
		if err != nil {
			return nil, false
		}
		paintStart := runes.at(paint.Start)
		start, end := runes.at(r.Start), runes.at(r.End)
		spans = append(spans, KeywordSpan{
			Word:  m.Word,
			Bytes: r,
			Runes: inputtext.Range{Start: start, End: end},
			Paint: inputtext.Range{Start: paintStart, End: runes.at(paint.End)},
		})
		previous = r.End
	}
	return spans, true
}

type runeCounter struct {
	text  string
	bytes int
	runes int
}

func (c *runeCounter) at(offset int) int {
	if offset < c.bytes {
		c.bytes, c.runes = 0, 0
	}
	c.runes += utf8.RuneCountInString(c.text[c.bytes:offset])
	c.bytes = offset
	return c.runes
}

func KeywordWords(spans []KeywordSpan) string {
	words := make([]string, 0, len(spans))
	for _, s := range spans {
		if !slices.Contains(words, s.Word) {
			words = append(words, s.Word)
		}
	}
	if len(words) == 0 {
		return ""
	}
	return "Keywords: " + strings.Join(words, ", ")
}
