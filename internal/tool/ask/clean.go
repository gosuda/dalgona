package ask

import (
	"strings"
	"unicode"
	"unicode/utf8"
)

func CleanLine(text string) string { return clean(text, false) }

func CleanBlock(text string) string { return clean(text, true) }

func clean(text string, multiline bool) string {
	text = strings.ToValidUTF8(text, "\uFFFD")
	var b strings.Builder
	for i := 0; i < len(text); {
		if text[i] == 0x1b {
			i = skipEscape(text, i)
			continue
		}
		r, size := utf8.DecodeRuneInString(text[i:])
		b.WriteString(keep(r, multiline))
		i += size
	}
	if multiline {
		return trimBlankLines(b.String())
	}
	return strings.TrimSpace(b.String())
}

func keep(r rune, multiline bool) string {
	switch {
	case r == '\n' && multiline:
		return "\n"
	case r == '\n', r == '\t' && !multiline:
		return " "
	case r == '\t':
		return "    "
	case isBidi(r) || unicode.IsControl(r):
		return ""
	default:
		return string(r)
	}
}

func isBidi(r rune) bool {
	return (r >= 0x202A && r <= 0x202E) || (r >= 0x2066 && r <= 0x2069)
}

func trimBlankLines(text string) string {
	for {
		line, rest, found := strings.Cut(text, "\n")
		if !found || strings.TrimSpace(line) != "" {
			return strings.TrimRightFunc(text, unicode.IsSpace)
		}
		text = rest
	}
}

func skipEscape(text string, start int) int {
	next := start + 1
	if next >= len(text) {
		return len(text)
	}
	switch text[next] {
	case '[':
		return skipCSI(text, next+1)
	case ']', 'P', 'X', '^', '_':
		return skipString(text, next+1)
	default:
		return skipShort(text, next)
	}
}

func skipCSI(text string, index int) int {
	for index < len(text) && !isCSIFinal(text[index]) {
		index++
	}
	return min(index+1, len(text))
}

func skipShort(text string, index int) int {
	for index < len(text) && text[index] >= 0x20 && text[index] <= 0x2f {
		index++
	}
	if index >= len(text) {
		return len(text)
	}
	_, size := utf8.DecodeRuneInString(text[index:])
	return index + size
}

func isCSIFinal(c byte) bool {
	return c >= 0x40 && c <= 0x7e
}

func skipString(text string, index int) int {
	for index < len(text) {
		if text[index] == 0x07 {
			return index + 1
		}
		if text[index] == 0x1b && index+1 < len(text) && text[index+1] == '\\' {
			return index + 2
		}
		index++
	}
	return len(text)
}
