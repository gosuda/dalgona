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
			if end, ok := completeEscape(text, i); ok {
				i = end
				continue
			}
			b.WriteString("\\u001B")
			i++
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

func completeEscape(text string, start int) (int, bool) {
	if start+1 >= len(text) {
		return 0, false
	}
	switch text[start+1] {
	case '[':
		return scanHeader(text, start+2)
	case ']':
		return scanOSC(text, start+2)
	case 'P':
		return scanDCS(text, start+2)
	case 'X', '^', '_':
		return scanPayloadST(text, start+2)
	default:
		return scanShort(text, start+1)
	}
}

func skipBytes(text string, index int, lo, hi byte) int {
	for index < len(text) && text[index] >= lo && text[index] <= hi {
		index++
	}
	return index
}

func scanHeader(text string, index int) (int, bool) {
	index = skipBytes(text, index, 0x30, 0x3f)
	index = skipBytes(text, index, 0x20, 0x2f)
	if index < len(text) && text[index] >= 0x40 && text[index] <= 0x7e {
		return index + 1, true
	}
	return 0, false
}

func scanShort(text string, index int) (int, bool) {
	for index < len(text) && text[index] >= 0x20 && text[index] <= 0x2f {
		index++
	}
	if index < len(text) && text[index] >= 0x30 && text[index] <= 0x7e {
		return index + 1, true
	}
	return 0, false
}

func scanOSC(text string, index int) (int, bool) {
	start := index
	index = skipBytes(text, index, '0', '9')
	if index == start {
		return 0, false
	}
	if index < len(text) && text[index] == 0x07 {
		return index + 1, true
	}
	if index+1 < len(text) && text[index] == 0x1b && text[index+1] == '\\' {
		return index + 2, true
	}
	if index >= len(text) || text[index] != ';' {
		return 0, false
	}
	return scanOSCPayload(text, index+1)
}

func scanDCS(text string, index int) (int, bool) {
	end, ok := scanHeader(text, index)
	if !ok {
		return 0, false
	}
	return scanPayloadST(text, end)
}

func scanPayloadST(text string, index int) (int, bool) {
	for index < len(text) {
		switch text[index] {
		case 0x18, 0x1a:
			return 0, false
		case 0x1b:
			if index+1 < len(text) && text[index+1] == '\\' {
				return index + 2, true
			}
			return 0, false
		}
		_, size := utf8.DecodeRuneInString(text[index:])
		index += size
	}
	return 0, false
}

func scanOSCPayload(text string, index int) (int, bool) {
	for index < len(text) {
		switch text[index] {
		case 0x07:
			return index + 1, true
		case 0x18, 0x1a:
			return 0, false
		case 0x1b:
			if index+1 < len(text) && text[index+1] == '\\' {
				return index + 2, true
			}
			return 0, false
		}
		_, size := utf8.DecodeRuneInString(text[index:])
		index += size
	}
	return 0, false
}
