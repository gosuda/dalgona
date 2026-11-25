package textinput

import "unicode/utf8"

type segmentEnd func(s string, start int) int

type localBreak func(s string, i int) bool

type absorber func(first, next scalarProperties) bool

type textUnit struct {
	end, peekSize int
	first, last   scalarProperties
	peek          scalarProperties
}

func segmentBoundaries(s string, end segmentEnd) []int {
	boundaries := []int{0}
	for start := 0; start < len(s); {
		start = end(s, start)
		boundaries = append(boundaries, start)
	}
	return boundaries
}

func nextSegmentBoundary(s string, i int, end segmentEnd, breaks localBreak) (int, error) {
	if !isScalarOffset(s, i) {
		return 0, InvalidOffset
	}
	if i == len(s) {
		return i, nil
	}
	start := safeStart(s, i, breaks)
	for start <= i {
		start = end(s, start)
	}
	return start, nil
}

func previousSegmentBoundary(s string, i int, end segmentEnd, breaks localBreak) (int, error) {
	if !isScalarOffset(s, i) {
		return 0, InvalidOffset
	}
	if i == 0 {
		return 0, nil
	}
	_, size := utf8.DecodeLastRuneInString(s[:i])
	previous := safeStart(s, i-size, breaks)
	for next := end(s, previous); next < i; next = end(s, next) {
		previous = next
	}
	return previous, nil
}

func isScalarOffset(s string, i int) bool {
	return i >= 0 && i <= len(s) && isScalarBoundary(s, i)
}

func safeStart(s string, i int, breaks localBreak) int {
	for i > 0 && !breaks(s, i) {
		_, size := utf8.DecodeLastRuneInString(s[:i])
		i -= size
	}
	return i
}

func unitAt(s string, start int, absorbs absorber) textUnit {
	r, size := utf8.DecodeRuneInString(s[start:])
	return grownUnit(s, start+size, lookup(r), absorbs)
}

func (u textUnit) successor(s string, absorbs absorber) textUnit {
	return grownUnit(s, u.end+u.peekSize, u.peek, absorbs)
}

func grownUnit(s string, end int, first scalarProperties, absorbs absorber) textUnit {
	u := textUnit{end: end, first: first, last: first}
	for u.end < len(s) {
		r, size := utf8.DecodeRuneInString(s[u.end:])
		next := lookup(r)
		if !absorbs(first, next) {
			u.peek, u.peekSize = next, size
			return u
		}
		u.last, u.end = next, u.end+size
	}
	return u
}
