package textinput

//go:generate go run gen.go

import (
	"sort"
	"unicode/utf8"
)

const UnicodeVersion = "18.0.0"

type propRange struct {
	lo, hi rune
	value  uint8
}

type gcb uint8

const (
	gcbOther gcb = iota
	gcbCR
	gcbLF
	gcbControl
	gcbExtend
	gcbZWJ
	gcbRI
	gcbPrepend
	gcbSpacingMark
	gcbL
	gcbV
	gcbT
	gcbLV
	gcbLVT
)

const (
	incbNone uint8 = iota
	incbLinker
	incbConsonant
	incbExtend
)

type props struct {
	gcb       gcb
	pict      bool
	incb      uint8
	ignorable bool
}

func lookup(r rune) props {
	i := sort.Search(len(propRanges), func(i int) bool { return propRanges[i].hi >= r })
	if i == len(propRanges) || propRanges[i].lo > r {
		return props{}
	}
	v := propRanges[i].value
	return props{gcb: gcb(v & 0x0f), pict: v&0x10 != 0, incb: v >> 5 & 0x03, ignorable: v&0x80 != 0}
}

type segmenter struct {
	started bool
	prev    gcb
	riRun   int
	pict    uint8
	linker  bool
}

func (s *segmenter) breaks(next props) bool {
	if !s.started {
		s.started = true
		s.advance(next)
		return true
	}
	brk := s.rule(next)
	s.advance(next)
	return brk
}

func (s *segmenter) rule(next props) bool {
	if brk, decided := pairRule(s.prev, next.gcb); decided {
		return brk
	}
	switch {
	case s.linker && next.incb == incbConsonant:
		return false
	case s.pict == 2 && next.pict:
		return false
	case s.prev == gcbRI && next.gcb == gcbRI:
		return s.riRun%2 == 0
	}
	return true
}

const (
	controls   = 1<<gcbCR | 1<<gcbLF | 1<<gcbControl
	attachNext = 1<<gcbExtend | 1<<gcbZWJ | 1<<gcbSpacingMark
)

var hangulNext = [...]uint16{
	gcbL:   1<<gcbL | 1<<gcbV | 1<<gcbLV | 1<<gcbLVT,
	gcbV:   1<<gcbV | 1<<gcbT,
	gcbLV:  1<<gcbV | 1<<gcbT,
	gcbT:   1 << gcbT,
	gcbLVT: 1 << gcbT,
}

func pairRule(prev, cur gcb) (brk, decided bool) {
	switch {
	case prev == gcbCR && cur == gcbLF:
		return false, true
	case controls&(1<<prev|1<<cur) != 0:
		return true, true
	case hangulNext[prev]&(1<<cur) != 0, attachNext&(1<<cur) != 0, prev == gcbPrepend:
		return false, true
	}
	return false, false
}

func (s *segmenter) advance(next props) {
	if next.gcb == gcbRI {
		s.riRun++
	} else {
		s.riRun = 0
	}
	s.pict = pictState(s.pict, next)
	if next.incb != incbExtend {
		s.linker = next.incb == incbLinker
	}
	s.prev = next.gcb
}

func pictState(state uint8, next props) uint8 {
	switch {
	case next.pict:
		return 1
	case state == 1 && next.gcb == gcbExtend:
		return 1
	case state == 1 && next.gcb == gcbZWJ:
		return 2
	}
	return 0
}

func Boundaries(s string) []int {
	out := []int{0}
	var seg segmenter
	for i, r := range s {
		if seg.breaks(lookup(r)) && i > 0 {
			out = append(out, i)
		}
	}
	if len(s) > 0 {
		out = append(out, len(s))
	}
	return out
}

func NextBoundary(s string, i int) int {
	if i >= len(s) {
		return len(s)
	}
	var seg segmenter
	for j, r := range s[i:] {
		if seg.breaks(lookup(r)) && j > 0 {
			return i + j
		}
	}
	return len(s)
}

func PrevBoundary(s string, i int) int {
	if i <= 0 {
		return 0
	}
	i = min(i, len(s))
	_, size := utf8.DecodeLastRuneInString(s[:i])
	last := restartPoint(s, i-size)
	for b := last; ; {
		next := NextBoundary(s, b)
		if next >= i {
			return last
		}
		last, b = next, next
	}
}

func restartPoint(s string, j int) int {
	for j > 0 {
		r, size := utf8.DecodeLastRuneInString(s[:j])
		if p := lookup(r).gcb; p == gcbLF || p == gcbControl {
			return j
		}
		j -= size
	}
	return 0
}

func IsBoundary(s string, i int) bool {
	if i == 0 || i == len(s) {
		return true
	}
	if i < 0 || i > len(s) || !utf8.RuneStart(s[i]) {
		return false
	}
	return NextBoundary(s, PrevBoundary(s, i)) == i
}
