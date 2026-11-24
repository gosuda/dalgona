package document

import (
	"unicode"
	"unicode/utf8"

	"github.com/gosuda/dalgona/internal/dal/inputtext"
)

type wordClass uint8

const (
	classSpace wordClass = iota + 1
	classWord
	classIdeograph
	classPunctuation
)

func classOf(cluster string) wordClass {
	r, _ := utf8.DecodeRuneInString(cluster)
	switch {
	case unicode.IsSpace(r):
		return classSpace
	case unicode.In(r, unicode.Han, unicode.Hiragana, unicode.Katakana):
		return classIdeograph
	case unicode.IsLetter(r) || unicode.IsDigit(r) || unicode.IsMark(r) || r == '_':
		return classWord
	default:
		return classPunctuation
	}
}

func nextCluster(text string, t int) int {
	next, err := inputtext.NextBoundary(text, t)
	if err != nil || next <= t {
		_, width := utf8.DecodeRuneInString(text[t:])
		return t + max(width, 1)
	}
	return next
}

func previousCluster(text string, t int) int {
	previous, err := inputtext.PreviousBoundary(text, t)
	if err != nil || previous >= t {
		_, width := utf8.DecodeLastRuneInString(text[:t])
		return t - max(width, 1)
	}
	return previous
}

func nextWordEnd(text string, t int) int {
	for t < len(text) && classOf(text[t:nextCluster(text, t)]) == classSpace {
		t = nextCluster(text, t)
	}
	if t == len(text) {
		return t
	}
	class := classOf(text[t:nextCluster(text, t)])
	t = nextCluster(text, t)
	for class != classIdeograph && t < len(text) && classOf(text[t:nextCluster(text, t)]) == class {
		t = nextCluster(text, t)
	}
	return t
}

func previousWordStart(text string, t int) int {
	for t > 0 && classOf(text[previousCluster(text, t):t]) == classSpace {
		t = previousCluster(text, t)
	}
	if t == 0 {
		return t
	}
	start := previousCluster(text, t)
	class := classOf(text[start:t])
	t = start
	for class != classIdeograph && t > 0 && classOf(text[previousCluster(text, t):t]) == class {
		t = previousCluster(text, t)
	}
	return t
}

func wordAround(text string, t int) (int, int) {
	if text == "" {
		return 0, 0
	}
	if t == len(text) {
		t = previousCluster(text, t)
	}
	end := nextCluster(text, t)
	class := classOf(text[t:end])
	if class == classIdeograph {
		return t, end
	}
	start := t
	for start > 0 && classOf(text[previousCluster(text, start):start]) == class {
		start = previousCluster(text, start)
	}
	for end < len(text) && classOf(text[end:nextCluster(text, end)]) == class {
		end = nextCluster(text, end)
	}
	return start, end
}
