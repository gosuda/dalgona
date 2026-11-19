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
	if strings.IndexByte(text, '\t') < 0 {
		return text
	}
	var out strings.Builder
	out.Grow(len(text) + tabStop)
	col := 0
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
	return s.method.StringWidth(s.nfc(s.expand(text)))
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
	return strings.Split(s.method.Wrap(s.nfc(s.expand(text)), n, ""), "\n")
}

type byteRange struct {
	start, end int
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
		pos = min(pos+len(part), len(flat))
		ranges[i] = byteRange{start: start, end: walk.advance(pos)}
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
