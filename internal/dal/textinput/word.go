package textinput

import "unicode/utf8"

func WordBoundaries(s string) []int {
	return segmentBoundaries(s, wordEnd)
}

func NextWordBoundary(s string, i int) (int, error) {
	return nextSegmentBoundary(s, i, wordEnd, wordBreaksAt)
}

func PreviousWordBoundary(s string, i int) (int, error) {
	return previousSegmentBoundary(s, i, wordEnd, wordBreaksAt)
}

const (
	wordAHLetter   uint32 = 1<<wordALetter | 1<<wordHebrewLetter
	wordLineBreaks uint32 = 1<<wordCR | 1<<wordLF | 1<<wordNewline
	wordIgnored    uint32 = 1<<wordExtend | 1<<wordFormat | 1<<wordZWJ
	wordSeparators uint32 = 1<<wordOther | 1<<wordWSegSpace
)

var wordFollowers = [wordWSegSpace + 1]uint32{
	wordALetter:      wordAHLetter | 1<<wordNumeric | 1<<wordExtendNumLet,
	wordHebrewLetter: wordAHLetter | 1<<wordNumeric | 1<<wordExtendNumLet | 1<<wordSingleQuote,
	wordNumeric:      wordAHLetter | 1<<wordNumeric | 1<<wordExtendNumLet,
	wordKatakana:     1<<wordKatakana | 1<<wordExtendNumLet,
	wordExtendNumLet: wordAHLetter | 1<<wordNumeric | 1<<wordKatakana | 1<<wordExtendNumLet,
}

var wordBridges = [...]struct{ outer, middle uint32 }{
	{wordAHLetter, 1<<wordMidLetter | 1<<wordMidNumLet | 1<<wordSingleQuote},
	{1 << wordHebrewLetter, 1 << wordDoubleQuote},
	{1 << wordNumeric, 1<<wordMidNum | 1<<wordMidNumLet | 1<<wordSingleQuote},
}

type wordWindow struct {
	before             wordClass
	previous           textUnit
	regionalIndicators int
}

func wordEnd(s string, start int) int {
	var w wordWindow
	w.enter(unitAt(s, start, wordAbsorbs))
	next := w.previous.successor(s, wordAbsorbs)
	for w.previous.end < len(s) {
		after := next.successor(s, wordAbsorbs)
		if !w.joins(next, after) {
			break
		}
		w.enter(next)
		next = after
	}
	return w.previous.end
}

func wordAbsorbs(first, next scalarProperties) bool {
	return !first.word.in(wordLineBreaks) && next.word.in(wordIgnored)
}

func (w *wordWindow) enter(u textUnit) {
	w.before = w.previous.first.word
	w.previous = u
	w.regionalIndicators++
	if u.first.word != wordRegionalIndicator {
		w.regionalIndicators = 0
	}
}

func (w *wordWindow) joins(next, after textUnit) bool {
	previous := w.previous.first.word
	if previous.in(wordLineBreaks) || next.first.word.in(wordLineBreaks) {
		return previous == wordCR && next.first.word == wordLF
	}
	return joinsScalars(w.previous.last, next.first) || w.joinsClasses(next.first.word, after.first.word)
}

func joinsScalars(last, first scalarProperties) bool {
	return last.word == wordZWJ && first.pictographic() || last.word == wordWSegSpace && first.word == wordWSegSpace
}

func (w *wordWindow) joinsClasses(next, after wordClass) bool {
	previous := w.previous.first.word
	return next.in(wordFollowers[previous]) || bridges(previous, next, after) || bridges(w.before, previous, next) ||
		next == wordRegionalIndicator && w.regionalIndicators%2 == 1
}

func bridges(left, middle, right wordClass) bool {
	for _, bridge := range wordBridges {
		if left.in(bridge.outer) && middle.in(bridge.middle) && right.in(bridge.outer) {
			return true
		}
	}
	return false
}

func wordBreaksAt(s string, i int) bool {
	before, _ := utf8.DecodeLastRuneInString(s[:i])
	at, _ := utf8.DecodeRuneInString(s[i:])
	left, right := lookup(before).word, lookup(at).word
	if left.in(wordLineBreaks) {
		return left != wordCR || right != wordLF
	}
	return left.in(wordSeparators) && !right.in(wordIgnored) && (left != wordWSegSpace || right != wordWSegSpace)
}
