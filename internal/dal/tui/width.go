package tui

import (
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/charmbracelet/x/ansi"
	"golang.org/x/text/unicode/norm"
)

func (s *screenState) nfc(text string) string {
	if norm.NFC.IsNormalString(text) {
		return text
	}
	return norm.NFC.String(text)
}

const tabStop = 8

func (s *screenState) expand(text string) string {
	return s.expandFrom(text, 0)
}

func (s *screenState) expandFrom(text string, col int) string {
	if strings.IndexByte(text, '\t') < 0 {
		return text
	}
	var out strings.Builder
	out.Grow(len(text) + tabStop)
	for text != "" {
		i := strings.IndexAny(text, "\t\n")
		if i < 0 {
			out.WriteString(text)
			break
		}
		col += s.method.StringWidth(s.nfc(text[:i]))
		out.WriteString(text[:i])
		if text[i] == '\n' {
			out.WriteByte('\n')
			col = 0
		} else {
			pad := tabStop - col%tabStop
			out.WriteString(strings.Repeat(" ", pad))
			col += pad
		}
		text = text[i+1:]
	}
	return out.String()
}

func (s *screenState) cells(text string) int {
	return s.cellsFrom(text, 0)
}

func (s *screenState) cellsFrom(text string, col int) int {
	return s.method.StringWidth(s.nfc(s.expandFrom(text, col)))
}

func (s *screenState) fit(text string, n int) string {
	nfc := s.nfc(s.expand(s.head(text, n)))
	if len(nfc) <= n {
		return nfc
	}
	if n < 3 {
		return s.truncate(nfc, n, "")
	}
	return s.truncate(nfc, n, "...")
}

func (s *screenState) head(text string, n int) string {
	for bound := max(n, 0)*8 + 64; ; bound *= 2 {
		head := plainPrefix(text, bound)
		if len(head) == len(text) || s.cells(head) > n {
			return head
		}
	}
}

func (s *screenState) truncate(text string, n int, tail string) string {
	if s.method.StringWidth(text) <= n {
		return text
	}
	keep := n - s.method.StringWidth(tail)
	var out strings.Builder
	used := 0
	cut := false
	for text != "" {
		seq, width := s.step(text)
		text = text[len(seq):]
		if cut && !strings.HasPrefix(seq, "\x1b") {
			continue
		}
		if !cut && width > 0 && used+width > keep {
			cut = true
			out.WriteString(tail)
			continue
		}
		used += width
		out.WriteString(seq)
	}
	return out.String()
}

func (s *screenState) step(text string) (string, int) {
	if c := text[0]; c >= ' ' && c < 0x7f || c >= 0xc0 {
		if cluster, width := ansi.FirstGraphemeCluster(text, s.method); cluster != "" {
			return cluster, width
		}
		return text[:1], 0
	}
	seq, _, _, _ := s.method.DecodeSequenceInString(text, 0, nil)
	if seq == "" {
		return text[:1], 0
	}
	return seq, 0
}

func plainPrefix(text string, bound int) string {
	if len(text) <= bound || strings.IndexByte(text, '\x1b') >= 0 {
		return text
	}
	for bound > 0 && !utf8.RuneStart(text[bound]) {
		bound--
	}
	return text[:bound]
}

func (s *screenState) wrap(text string, n int) []string {
	flat := s.nfc(s.expand(text))
	var rows []string
	for {
		parts := strings.Split(s.method.Wrap(flat, n, ""), "\n")
		over, head, rest := s.firstOverflow(flat, parts, n)
		if over < 0 {
			if rows == nil {
				return parts
			}
			return append(rows, parts...)
		}
		rows = append(append(rows, parts[:over]...), head)
		if rest == "" {
			return rows
		}
		flat = rest
	}
}

func (s *screenState) firstOverflow(flat string, parts []string, n int) (int, string, string) {
	if n < 1 {
		return -1, "", ""
	}
	for i, part := range parts {
		if s.method.StringWidth(part) <= n {
			continue
		}
		cut, resume, ok := s.splitRow(part, n)
		if !ok {
			continue
		}
		if pos := s.rowOffset(flat, parts, i); strings.HasPrefix(flat[pos:], part) {
			return i, part[:cut], flat[pos+resume:]
		}
	}
	return -1, "", ""
}

func (s *screenState) rowOffset(flat string, parts []string, index int) int {
	pos := 0
	for _, part := range parts[:index] {
		pos = min(s.rowStart(flat, pos, part)+len(part), len(flat))
	}
	return s.rowStart(flat, pos, parts[index])
}

func (s *screenState) splitRow(row string, n int) (cut, resume int, ok bool) {
	at, runStart, runEnd := s.scanRow(row, n)
	switch {
	case at < 0:
		return 0, 0, false
	case runStart < 0:
		return at, at, at > 0
	}
	return runStart, s.spaceEnd(row, runEnd), true
}

func (s *screenState) scanRow(row string, n int) (at, runStart, runEnd int) {
	used, open := 0, -1
	runStart, runEnd = -1, -1
	for at < len(row) {
		seq, width := s.step(row[at:])
		if width > 0 && wrapSpace(seq) {
			if open < 0 {
				open = at
			}
			runStart, runEnd = open, at+len(seq)
		} else {
			open = -1
		}
		if used+width > n {
			return at, runStart, runEnd
		}
		used += width
		at += len(seq)
	}
	return -1, -1, -1
}

func (s *screenState) spaceEnd(row string, at int) int {
	for at < len(row) {
		seq, width := s.step(row[at:])
		if width == 0 || !wrapSpace(seq) {
			break
		}
		at += len(seq)
	}
	return at
}

func wrapSpace(cluster string) bool {
	r, _ := utf8.DecodeRuneInString(cluster)
	return r != utf8.RuneError && r != 0xa0 && unicode.IsSpace(r)
}

type byteRange struct {
	start, end, col int
}

func (s *screenState) wrapRanges(text string, n int) []byteRange {
	flat := s.nfc(s.expand(text))
	parts := s.wrap(flat, n)
	ranges := make([]byteRange, len(parts))
	walk := sourceWalk{screen: s, text: text}
	pos := 0
	for i, part := range parts {
		pos = s.rowStart(flat, pos, part)
		start := walk.advance(pos)
		col := walk.col
		pos = min(pos+len(part), len(flat))
		ranges[i] = byteRange{start: start, end: walk.advance(pos), col: col}
	}
	return ranges
}

func (s *screenState) rowStart(flat string, pos int, part string) int {
	for at := pos; ; {
		if strings.HasPrefix(flat[at:], part) {
			return at
		}
		next := s.skipSpace(flat, at)
		if next == at {
			return pos
		}
		at = next
	}
}

func (s *screenState) skipSpace(text string, pos int) int {
	if pos >= len(text) {
		return pos
	}
	r, size := utf8.DecodeRuneInString(text[pos:])
	if !unicode.IsSpace(r) {
		return pos
	}
	if size > 1 {
		cluster, _ := ansi.FirstGraphemeCluster(text[pos:], s.method)
		size = len(cluster)
	}
	return pos + size
}

func printableASCII(text string, at int) bool {
	b := text[at]
	if b < ' ' || b > '~' {
		return false
	}
	return at+1 == len(text) || text[at+1] < utf8.RuneSelf
}

type sourceWalk struct {
	screen *screenState
	text   string
	at     int
	flat   int
	col    int
}

func (w *sourceWalk) advance(flat int) int {
	for w.flat < flat && w.at < len(w.text) {
		if printableASCII(w.text, w.at) {
			w.at++
			w.flat++
			w.col++
			continue
		}
		cluster, _ := ansi.FirstGraphemeCluster(w.text[w.at:], w.screen.method)
		if cluster == "\t" {
			pad := tabStop - w.col%tabStop
			w.flat += pad
			w.col += pad
		} else {
			composed := w.screen.nfc(cluster)
			w.flat += len(composed)
			w.col += w.screen.method.StringWidth(composed)
		}
		w.at += len(cluster)
	}
	return w.at
}
