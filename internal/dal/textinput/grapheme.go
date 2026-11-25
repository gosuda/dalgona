package textinput

import "unicode/utf8"

const UnicodeVersion = "18.0.0"

func NextBoundary(s string, i int) int {
	if i < 0 {
		i = 0
	}
	if i >= len(s) {
		return len(s)
	}
	if end := boundaryAtOrAfter(s, i); end != i {
		return end
	}
	return clusterEnd(s, i)
}

func PrevBoundary(s string, i int) int {
	if i <= 0 {
		return 0
	}
	if i > len(s) {
		i = len(s)
	}
	start, _ := clusterAt(s, i-1)
	return start
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
