package textinput

import "unicode/utf8"

func SentenceBoundaries(s string) []int {
	return segmentBoundaries(s, sentenceEnd)
}

func NextSentenceBoundary(s string, i int) (int, error) {
	return nextSegmentBoundary(s, i, sentenceEnd, sentenceBreaksAt)
}

func PreviousSentenceBoundary(s string, i int) (int, error) {
	return previousSegmentBoundary(s, i, sentenceEnd, sentenceBreaksAt)
}

const (
	sentenceParagraphSeparators uint32 = 1<<sentenceSep | 1<<sentenceCR | 1<<sentenceLF
	sentenceTerminators         uint32 = 1<<sentenceATerm | 1<<sentenceSTerm
	sentenceIgnored             uint32 = 1<<sentenceExtend | 1<<sentenceFormat
	sentenceCased               uint32 = 1<<sentenceUpper | 1<<sentenceLower
	sentenceContinuers          uint32 = sentenceParagraphSeparators | sentenceTerminators | 1<<sentenceSContinue | 1<<sentenceSp
	sentenceLowerStops          uint32 = sentenceParagraphSeparators | sentenceTerminators | sentenceCased | 1<<sentenceOLetter
	sentenceOpeners             uint32 = 1<<sentenceUpper | 1<<sentenceOLetter
)

type terminatorTail uint8

const (
	noTerminator terminatorTail = iota
	afterTerminator
	afterClose
	afterSpace
)

type sentenceWindow struct {
	previous, terminator, beforeTerminator sentenceClass
	tail                                   terminatorTail
}

func sentenceEnd(s string, start int) int {
	var w sentenceWindow
	current := unitAt(s, start, sentenceAbsorbs)
	w.enter(current.first.sentence)
	for current.end < len(s) {
		next := current.successor(s, sentenceAbsorbs)
		if !w.joins(s, next) {
			break
		}
		w.enter(next.first.sentence)
		current = next
	}
	return current.end
}

func sentenceAbsorbs(first, next scalarProperties) bool {
	return !first.sentence.in(sentenceParagraphSeparators) && next.sentence.in(sentenceIgnored)
}

func (w *sentenceWindow) enter(class sentenceClass) {
	switch {
	case class.in(sentenceTerminators):
		w.terminator, w.beforeTerminator, w.tail = class, w.previous, afterTerminator
	case class == sentenceClose && (w.tail == afterTerminator || w.tail == afterClose):
		w.tail = afterClose
	case class == sentenceSp && w.tail != noTerminator:
		w.tail = afterSpace
	default:
		w.tail = noTerminator
	}
	w.previous = class
}

func (w *sentenceWindow) joins(s string, next textUnit) bool {
	class := next.first.sentence
	switch {
	case w.previous.in(sentenceParagraphSeparators):
		return w.previous == sentenceCR && class == sentenceLF
	case w.tail == noTerminator, class.in(sentenceContinuers):
		return true
	case class == sentenceClose && w.tail != afterSpace:
		return true
	}
	return w.terminator == sentenceATerm && w.joinsFullStop(s, next)
}

func (w *sentenceWindow) joinsFullStop(s string, next textUnit) bool {
	class := next.first.sentence
	if w.tail == afterTerminator && (class == sentenceNumeric || class == sentenceUpper && w.beforeTerminator.in(sentenceCased)) {
		return true
	}
	return lowerFollows(s, next)
}

func lowerFollows(s string, u textUnit) bool {
	for !u.first.sentence.in(sentenceLowerStops) && u.end < len(s) {
		u = u.successor(s, sentenceAbsorbs)
	}
	return u.first.sentence == sentenceLower
}

func sentenceBreaksAt(s string, i int) bool {
	before, _ := utf8.DecodeLastRuneInString(s[:i])
	if lookup(before).sentence.in(sentenceParagraphSeparators) {
		return before != '\r' || s[i] != '\n'
	}
	at, _ := utf8.DecodeRuneInString(s[i:])
	opener := lookup(at).sentence
	return opener.in(sentenceOpeners) && terminatedBefore(s, i, opener)
}

func terminatedBefore(s string, i int, opener sentenceClass) bool {
	end := trimClass(s, trimClass(s, i, sentenceSp), sentenceClose)
	terminator, _ := utf8.DecodeLastRuneInString(s[:end])
	class := lookup(terminator).sentence
	return end > 0 && (class == sentenceSTerm || class == sentenceATerm && (opener == sentenceOLetter || end < i))
}

func trimClass(s string, i int, class sentenceClass) int {
	for i > 0 {
		r, size := utf8.DecodeLastRuneInString(s[:i])
		if lookup(r).sentence != class {
			return i
		}
		i -= size
	}
	return 0
}
