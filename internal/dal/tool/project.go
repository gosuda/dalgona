package tool

import (
	"strconv"
	"strings"
	"unicode/utf8"
)

const projectionLimit = 64 * 1024

func Project(out []byte, keep Keep, spill string) (string, bool) {
	if len(out) <= projectionLimit {
		return string(out), false
	}
	part := projectionWindow(out, keep)
	total := strconv.Itoa(len(out))
	const prefix, middle, suffix = "\n[output truncated: ", " bytes total; full output: ", "; page it with read offset/limit]"
	var b strings.Builder
	b.Grow(len(part) + len(prefix) + len(total) + len(middle) + len(spill) + len(suffix))
	b.Write(part)
	b.WriteString(prefix)
	b.WriteString(total)
	b.WriteString(middle)
	b.WriteString(spill)
	b.WriteString(suffix)
	return b.String(), true
}

func projectionWindow(out []byte, keep Keep) []byte {
	if keep == KeepTail {
		return trimToRuneStart(out[len(out)-projectionLimit:])
	}
	return trimPartialRune(out[:projectionLimit])
}

func trimPartialRune(data []byte) []byte {
	start := max(0, len(data)-utf8.UTFMax+1)
	last := -1
	for index := start; index < len(data); index++ {
		if utf8.RuneStart(data[index]) {
			last = index
		}
	}
	if last >= 0 && !utf8.FullRune(data[last:]) {
		return data[:last]
	}
	return data
}

func trimToRuneStart(data []byte) []byte {
	index := 0
	for index < len(data) && index < utf8.UTFMax-1 && !utf8.RuneStart(data[index]) {
		index++
	}
	return data[index:]
}
