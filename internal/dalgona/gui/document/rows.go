package document

import (
	"strings"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

func rowsOf(kind Kind, text string) ([]Span, bool) {
	switch kind {
	case KindCode, KindFigure, KindHTML, KindNotice, KindDiff:
		return lineRows(text, 0, nil)
	case KindParagraph, KindHeading, KindMath, KindTable, KindRule:
		return boundedRows(text, Span{Start: 0, End: len(text)}, nil)
	}
	return []Span{{}}, false
}

func lineRows(text string, from int, rows []Span) ([]Span, bool) {
	if from == len(text) && len(rows) > 0 {
		return rows, false
	}
	wrapped := false
	for start := from; ; {
		end := len(text)
		if newline := strings.IndexByte(text[start:], '\n'); newline >= 0 {
			end = start + newline + 1
		}
		var split bool
		rows, split = boundedRows(text, Span{Start: start, End: end}, rows)
		wrapped = wrapped || split
		if end == len(text) {
			return rows, wrapped
		}
		start = end
	}
}

func boundedRows(text string, line Span, rows []Span) ([]Span, bool) {
	if line.Len() <= MaxRowBytes {
		return append(rows, line), false
	}
	for start := line.Start; start < line.End; {
		end := rowEnd(text, start, line.End)
		rows = append(rows, Span{Start: start, End: end})
		start = end
	}
	return rows, true
}

func rowEnd(text string, start, limit int) int {
	if limit-start <= MaxRowBytes {
		return limit
	}
	line := text[start:limit]
	cut := MaxRowBytes
	for !utf8.RuneStart(line[cut]) {
		cut--
	}
	grapheme, err := inputtext.PaintRange(line, inputtext.Range{Start: cut, End: cut})
	if err != nil {
		return limit
	}
	if grapheme.Start == 0 {
		return start + grapheme.End
	}
	return start + grapheme.Start
}
