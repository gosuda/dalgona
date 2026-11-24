package tui

import (
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

const (
	tabStop = 8
	nbsp    = '\u00a0'
)

func (s *screenState) expand(text string) string {
	return s.expandFrom(text, 0)
}

func (s *screenState) expandDisplay(text string) string {
	return s.expandFrom(displayText(text), 0)
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
		col += s.method.StringWidth(text[:i])
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
	return s.method.StringWidth(s.expandFrom(text, col))
}

func (s *screenState) fit(text string, n int) string {
	expanded := s.expand(s.head(text, n))
	if len(expanded) <= n {
		return expanded
	}
	if n < 3 {
		return s.truncate(expanded, n, "")
	}
	return s.truncate(expanded, n, "...")
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
	if isDisplayEscape(text) {
		return text[:8], s.method.StringWidth(text[:8])
	}
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
	text = s.expand(text)
	if strings.Contains(text, "<U+") {
		return s.greedyWrap(text, n)
	}
	if n < 1 {
		return strings.Split(s.method.Wrap(text, n, ""), "\n")
	}
	var rows []string
	for line := range strings.SplitSeq(text, "\n") {
		rows = append(rows, s.wrapLine(line, n)...)
	}
	return rows
}

func (s *screenState) greedyWrap(text string, n int) []string {
	if n < 1 {
		return strings.Split(s.method.Wrap(text, n, ""), "\n")
	}
	var rows []string
	for line := range strings.SplitSeq(text, "\n") {
		rows = append(rows, s.greedyRows(line, n)...)
	}
	return rows
}

func isDisplayEscape(text string) bool {
	if !hasDisplayPrefix(text) {
		return false
	}
	for i := 3; i < 7; i++ {
		if !isDisplayHexDigit(text[i]) {
			return false
		}
	}
	return true
}

func hasDisplayPrefix(text string) bool {
	return len(text) >= 8 && text[0] == '<' && text[1] == 'U' && text[2] == '+' && text[7] == '>'
}

func isDisplayHexDigit(c byte) bool {
	return c >= '0' && c <= '9' || c >= 'A' && c <= 'F'
}

func (s *screenState) wrapLine(line string, n int) []string {
	rows := strings.Split(s.method.Wrap(line, n, ""), "\n")
	if s.rowsFit(rows, n) && s.rowsKeep(line, rows) {
		return rows
	}
	return s.greedyRows(line, n)
}

func (s *screenState) rowsFit(rows []string, n int) bool {
	for i, row := range rows {
		if s.method.StringWidth(row) > n {
			return false
		}
		if i > 0 && row != "" && s.startsInsideCluster(row) {
			return false
		}
	}
	return true
}

func (s *screenState) rowsKeep(line string, rows []string) bool {
	pos := 0
	for _, row := range rows {
		for !strings.HasPrefix(line[pos:], row) {
			next := s.skipSpace(line, pos)
			if next == pos {
				return false
			}
			pos = next
		}
		pos += len(row)
	}
	for pos < len(line) {
		next := s.skipSpace(line, pos)
		if next == pos {
			return false
		}
		pos = next
	}
	return true
}

func (s *screenState) startsInsideCluster(row string) bool {
	seq, width := s.step(row)
	return width == 0 && !strings.HasPrefix(seq, "\x1b")
}

type rowBuilder struct {
	rows   []string
	row    strings.Builder
	space  strings.Builder
	word   strings.Builder
	rowW   int
	spaceW int
	wordW  int
	limit  int
}

func (b *rowBuilder) flush() {
	b.rows = append(b.rows, b.row.String())
	b.row.Reset()
	b.rowW = 0
}

func (b *rowBuilder) place() {
	if b.word.Len() == 0 {
		return
	}
	fits := b.rowW+b.spaceW+b.wordW <= b.limit
	switch {
	case b.rowW > 0 && !fits:
		b.flush()
	case fits:
		b.row.WriteString(b.space.String())
		b.rowW += b.spaceW
	}
	b.row.WriteString(b.word.String())
	b.rowW += b.wordW
	b.space.Reset()
	b.spaceW = 0
	b.word.Reset()
	b.wordW = 0
}

func (b *rowBuilder) breakWord() {
	if b.rowW > 0 {
		b.flush()
	}
	b.row.WriteString(b.word.String())
	b.rowW = b.wordW
	b.flush()
	b.space.Reset()
	b.spaceW = 0
	b.word.Reset()
	b.wordW = 0
}

func (s *screenState) greedyRows(line string, n int) []string {
	b := rowBuilder{limit: n}
	for line != "" {
		seq, width := s.step(line)
		line = line[len(seq):]
		if r, _ := utf8.DecodeRuneInString(seq); r != utf8.RuneError && unicode.IsSpace(r) && r != nbsp {
			b.place()
			b.space.WriteString(seq)
			b.spaceW += width
			continue
		}
		if b.wordW > 0 && b.wordW+width > n {
			b.breakWord()
		}
		b.word.WriteString(seq)
		b.wordW += width
	}
	b.place()
	if b.row.Len() > 0 || len(b.rows) == 0 {
		b.flush()
	}
	return b.rows
}

type byteRange struct {
	start, end, col int
}

func (s *screenState) wrapRanges(text string, n int) []byteRange {
	flat := s.expandDisplay(text)
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
	r, _ := utf8.DecodeRuneInString(text[pos:])
	if !unicode.IsSpace(r) {
		return pos
	}
	cluster, _ := ansi.FirstGraphemeCluster(text[pos:], s.method)
	return pos + len(cluster)
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
		size, _ := inputtext.NextBoundary(w.text[w.at:], 0)
		cluster := w.text[w.at : w.at+size]
		if cluster == "\t" {
			pad := tabStop - w.col%tabStop
			w.flat += pad
			w.col += pad
			w.at++
			continue
		}
		shown := controlDisplay(cluster)
		w.flat += len(shown)
		w.col += w.screen.method.StringWidth(shown)
		w.at += len(cluster)
	}
	return w.at
}
