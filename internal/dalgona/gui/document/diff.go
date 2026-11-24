package document

import (
	"sort"
	"strings"
	"unicode/utf8"
)

type Edit struct {
	OldStart int
	OldEnd   int
	NewStart int
	NewEnd   int
}

type lineSide struct {
	text  string
	lines []Span
	base  int
}

func (s lineSide) line(i int) string {
	return s.text[s.lines[i].Start:s.lines[i].End]
}

func (s lineSide) slice(from, to int) lineSide {
	base := s.base
	if from < len(s.lines) {
		base = s.lines[from].Start
	} else if len(s.lines) > 0 {
		base = s.lines[len(s.lines)-1].End
	}
	return lineSide{text: s.text, lines: s.lines[from:to], base: base}
}

func (s lineSide) end() int {
	if len(s.lines) == 0 {
		return s.base
	}
	return s.lines[len(s.lines)-1].End
}

func splitLines(text string) []Span {
	var lines []Span
	for start := 0; start < len(text); {
		end := len(text)
		if newline := strings.IndexByte(text[start:], '\n'); newline >= 0 {
			end = start + newline + 1
		}
		lines = append(lines, Span{Start: start, End: end})
		start = end
	}
	return lines
}

func diffEdits(before, after string) []Edit {
	var edits []Edit
	diffSides(lineSide{text: before, lines: splitLines(before)}, lineSide{text: after, lines: splitLines(after)}, &edits)
	return edits
}

func commonLinePrefix(a, b lineSide) int {
	prefix := 0
	for prefix < len(a.lines) && prefix < len(b.lines) && a.line(prefix) == b.line(prefix) {
		prefix++
	}
	return prefix
}

func commonLineSuffix(a, b lineSide) int {
	suffix := 0
	for suffix < len(a.lines) && suffix < len(b.lines) && a.line(len(a.lines)-1-suffix) == b.line(len(b.lines)-1-suffix) {
		suffix++
	}
	return suffix
}

func diffAnchored(a, b lineSide, anchors [][2]int, edits *[]Edit) {
	previousA, previousB := 0, 0
	for _, anchor := range anchors {
		diffSides(a.slice(previousA, anchor[0]), b.slice(previousB, anchor[1]), edits)
		previousA, previousB = anchor[0]+1, anchor[1]+1
	}
	diffSides(a.slice(previousA, len(a.lines)), b.slice(previousB, len(b.lines)), edits)
}

func diffSides(a, b lineSide, edits *[]Edit) {
	prefix := commonLinePrefix(a, b)
	a, b = a.slice(prefix, len(a.lines)), b.slice(prefix, len(b.lines))
	suffix := commonLineSuffix(a, b)
	a, b = a.slice(0, len(a.lines)-suffix), b.slice(0, len(b.lines)-suffix)
	if len(a.lines) == 0 && len(b.lines) == 0 {
		return
	}
	anchors := uniqueAnchors(a, b)
	if len(anchors) == 0 {
		addEdit(edits, refine(a, b))
		return
	}
	diffAnchored(a, b, anchors, edits)
}

func uniqueAnchors(a, b lineSide) [][2]int {
	counts := map[string][2]int{}
	positions := map[string][2]int{}
	for i := range a.lines {
		c := counts[a.line(i)]
		c[0]++
		counts[a.line(i)] = c
		positions[a.line(i)] = [2]int{i, positions[a.line(i)][1]}
	}
	for j := range b.lines {
		c := counts[b.line(j)]
		c[1]++
		counts[b.line(j)] = c
		positions[b.line(j)] = [2]int{positions[b.line(j)][0], j}
	}
	var pairs [][2]int
	for line, c := range counts {
		if c[0] == 1 && c[1] == 1 && strings.TrimSpace(line) != "" {
			pairs = append(pairs, positions[line])
		}
	}
	sort.Slice(pairs, func(i, j int) bool { return pairs[i][0] < pairs[j][0] })
	return increasing(pairs)
}

func increasing(pairs [][2]int) [][2]int {
	if len(pairs) == 0 {
		return nil
	}
	tails, links := []int{}, make([]int, len(pairs))
	for i, pair := range pairs {
		k := sort.Search(len(tails), func(k int) bool { return pairs[tails[k]][1] >= pair[1] })
		links[i] = -1
		if k > 0 {
			links[i] = tails[k-1]
		}
		if k == len(tails) {
			tails = append(tails, i)
		} else {
			tails[k] = i
		}
	}
	out := make([][2]int, len(tails))
	for i, k := tails[len(tails)-1], len(tails)-1; k >= 0; i, k = links[i], k-1 {
		out[k] = pairs[i]
	}
	return out
}

func commonBytePrefix(oldText, newText string) int {
	prefix := 0
	for prefix < len(oldText) && prefix < len(newText) && oldText[prefix] == newText[prefix] {
		prefix++
	}
	return prefix
}

func alignPrefixToRune(newText string, prefix int) int {
	for prefix > 0 && prefix < len(newText) && !utf8.RuneStart(newText[prefix]) {
		prefix--
	}
	return prefix
}

func commonByteSuffix(oldText, newText string, prefix int) int {
	suffix := 0
	for suffix < len(oldText)-prefix && suffix < len(newText)-prefix && oldText[len(oldText)-1-suffix] == newText[len(newText)-1-suffix] {
		suffix++
	}
	return suffix
}

func alignSuffixToRune(newText string, suffix int) int {
	for suffix > 0 && !utf8.RuneStart(newText[len(newText)-suffix]) {
		suffix--
	}
	return suffix
}

func refine(a, b lineSide) Edit {
	oldText, newText := a.text[a.base:a.end()], b.text[b.base:b.end()]
	prefix := alignPrefixToRune(newText, commonBytePrefix(oldText, newText))
	suffix := alignSuffixToRune(newText, commonByteSuffix(oldText, newText, prefix))
	return Edit{
		OldStart: a.base + prefix,
		OldEnd:   a.end() - suffix,
		NewStart: b.base + prefix,
		NewEnd:   b.end() - suffix,
	}
}

func addEdit(edits *[]Edit, e Edit) {
	if e.OldStart == e.OldEnd && e.NewStart == e.NewEnd {
		return
	}
	if n := len(*edits); n > 0 && (*edits)[n-1].OldEnd == e.OldStart {
		(*edits)[n-1].OldEnd, (*edits)[n-1].NewEnd = e.OldEnd, e.NewEnd
		return
	}
	*edits = append(*edits, e)
}

func mapOffset(edits []Edit, offset int, affinity Affinity) int {
	i := sort.Search(len(edits), func(i int) bool { return edits[i].OldEnd >= offset })
	if i == len(edits) {
		return mapOffsetBeyond(edits, offset)
	}
	return mapOffsetAt(edits[i], offset, affinity)
}

func mapOffsetBeyond(edits []Edit, offset int) int {
	if len(edits) == 0 {
		return offset
	}
	last := edits[len(edits)-1]
	return offset + last.NewEnd - last.OldEnd
}

func mapOffsetAt(e Edit, offset int, affinity Affinity) int {
	switch {
	case offset < e.OldStart:
		return offset + e.NewStart - e.OldStart
	case offset == e.OldEnd && e.OldEnd > e.OldStart:
		return e.NewEnd
	case affinity == After && (offset > e.OldStart || e.OldEnd == e.OldStart):
		return e.NewEnd
	default:
		return e.NewStart
	}
}
