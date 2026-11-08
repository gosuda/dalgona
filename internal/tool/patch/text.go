package patch

import (
	"bytes"
	"fmt"
	"sort"
	"strings"
	"unicode/utf8"
)

type document struct {
	raw          []byte
	view         []byte
	bom          bool
	removedCR    []int
	finalNewline bool
	dominantCRLF bool
	starts       []int
}

func decodeDocument(path string, raw []byte) (document, error) {
	if len(raw) > 16<<20 {
		return document{}, fmt.Errorf("patch: %s is over 16 MiB. Use exec", path)
	}
	if bytes.IndexByte(raw[:min(len(raw), 8192)], 0) >= 0 || !utf8.Valid(raw) {
		return document{}, fmt.Errorf("patch: %s is not a text file (binary or invalid UTF-8)", path)
	}
	doc := document{raw: raw, bom: bytes.HasPrefix(raw, []byte{0xef, 0xbb, 0xbf})}
	text := raw
	if doc.bom {
		text = raw[3:]
	}
	doc.view, doc.removedCR = normalizeLineEnds(text)
	crlf := len(doc.removedCR)
	doc.finalNewline = len(doc.view) > 0 && doc.view[len(doc.view)-1] == '\n'
	doc.dominantCRLF = crlf > bytes.Count(text, []byte{'\n'})-crlf
	doc.starts = lineStarts(doc.view)
	return doc, nil
}

func normalizeLineEnds(text []byte) ([]byte, []int) {
	view := make([]byte, 0, len(text))
	var removedCR []int
	for i, char := range text {
		if char == '\r' && i+1 < len(text) && text[i+1] == '\n' {
			removedCR = append(removedCR, len(view))
			continue
		}
		view = append(view, char)
	}
	return view, removedCR
}

func lineStarts(view []byte) []int {
	if len(view) == 0 {
		return nil
	}
	starts := []int{0}
	for i, char := range view {
		if char == '\n' && i+1 < len(view) {
			starts = append(starts, i+1)
		}
	}
	return starts
}

func (d document) lines() int { return len(d.starts) }

func (d document) rawOffset(offset int) int {
	removed := sort.SearchInts(d.removedCR, offset)
	if d.bom {
		return offset + removed + 3
	}
	return offset + removed
}

func (d document) lineRange(first, last int) (int, int) {
	start := d.viewLen()
	if first >= 1 && first <= len(d.starts) {
		start = d.starts[first-1]
	}
	end := d.viewLen()
	if last >= 1 && last < len(d.starts) {
		end = d.starts[last]
	}
	return d.rawOffset(start), d.rawOffset(end)
}

func (d document) lineText(line int) string {
	start, end := d.starts[line-1], d.viewLen()
	if line < len(d.starts) {
		end = d.starts[line]
	}
	return strings.TrimSuffix(string(d.view[start:end]), "\n")
}

func (d document) viewLen() int { return len(d.view) }

func (d document) gapOffset(gap int) int {
	if gap >= len(d.starts) {
		return len(d.raw)
	}
	return d.rawOffset(d.starts[gap])
}

func (d document) render(body string) []byte {
	if d.dominantCRLF {
		return []byte(strings.ReplaceAll(body, "\n", "\r\n"))
	}
	return []byte(body)
}

func countBodyLines(body []byte) int {
	if len(body) == 0 {
		return 0
	}
	count := bytes.Count(body, []byte{'\n'})
	if body[len(body)-1] != '\n' {
		count++
	}
	return count
}
