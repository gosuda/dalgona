package textinput

import (
	"fmt"
	"strings"
	"unicode/utf8"
)

const (
	esc = 0x1b
	bel = 0x07
	can = 0x18
	sub = 0x1a
)

func EscapeEnd(s string, i int) (int, bool) {
	if i < 0 || i+1 >= len(s) || s[i] != esc {
		return 0, false
	}
	k := i + 2
	switch s[i+1] {
	case '[':
		return header(s, k)
	case 'P':
		return dcsEnd(s, k)
	case ']':
		return oscEnd(s, k)
	case 'X', '^', '_':
		return payloadEnd(s, k, false)
	}
	return shortEnd(s, i+1)
}

func skip(s string, k int, lo, hi byte) int {
	for k < len(s) && s[k] >= lo && s[k] <= hi {
		k++
	}
	return k
}

func final(s string, k int, lo byte) (int, bool) {
	if k < len(s) && s[k] >= lo && s[k] <= 0x7e {
		return k + 1, true
	}
	return 0, false
}

func header(s string, k int) (int, bool) {
	return final(s, skip(s, skip(s, k, 0x30, 0x3f), 0x20, 0x2f), 0x40)
}

func shortEnd(s string, k int) (int, bool) {
	return final(s, skip(s, k, 0x20, 0x2f), 0x30)
}

func dcsEnd(s string, k int) (int, bool) {
	end, ok := header(s, k)
	if !ok {
		return 0, false
	}
	return payloadEnd(s, end, false)
}

func oscEnd(s string, k int) (int, bool) {
	end := skip(s, k, '0', '9')
	if end == k {
		return 0, false
	}
	if end < len(s) && s[end] == ';' {
		return payloadEnd(s, end+1, true)
	}
	return terminatorEnd(s, end, true)
}

func payloadEnd(s string, k int, bell bool) (int, bool) {
	for k < len(s) {
		switch s[k] {
		case esc, bel:
			return terminatorEnd(s, k, bell)
		case can, sub:
			return 0, false
		}
		r, size := utf8.DecodeRuneInString(s[k:])
		if r == utf8.RuneError && size == 1 {
			return 0, false
		}
		k += size
	}
	return 0, false
}

func terminatorEnd(s string, k int, bell bool) (int, bool) {
	switch {
	case k < len(s) && s[k] == bel && bell:
		return k + 1, true
	case k+1 < len(s) && s[k] == esc && s[k+1] == '\\':
		return k + 2, true
	}
	return 0, false
}

func Eligible(s string) bool {
	for i := 0; i < len(s); {
		if end, ok := EscapeEnd(s, i); ok {
			i = end
			continue
		}
		r, size := utf8.DecodeRuneInString(s[i:])
		if !IsWhiteSpace(r) && !isControl(r) && lookup(r).property&defaultIgnorable == 0 {
			return true
		}
		i += size
	}
	return false
}

func isControl(r rune) bool { return r < 0x20 || r >= 0x7f && r <= 0x9f }

func isBidiControl(r rune) bool {
	return r == 0x061c || r == 0x200e || r == 0x200f || r >= 0x202a && r <= 0x202e || r >= 0x2066 && r <= 0x2069
}

func Visible(r rune) string { return fmt.Sprintf("<U+%04X>", r) }

func DisplayLine(s string) string { return strings.TrimSpace(project(s, false)) }

func DisplayBlock(s string) string {
	text := project(s, true)
	for {
		line, rest, found := strings.Cut(text, "\n")
		if !found || strings.TrimSpace(line) != "" {
			return strings.TrimRight(text, " ")
		}
		text = rest
	}
}

func project(s string, block bool) string {
	var b strings.Builder
	for i := 0; i < len(s); {
		if end, ok := EscapeEnd(s, i); ok {
			i = end
			continue
		}
		r, size := utf8.DecodeRuneInString(s[i:])
		if r == '\r' && strings.HasPrefix(s[i+1:], "\n") {
			size++
			r = '\n'
		}
		b.WriteString(displayRune(r, block))
		i += size
	}
	return b.String()
}

func displayRune(r rune, block bool) string {
	switch {
	case IsLineBreak(r) && block:
		return "\n"
	case r == '\t' && block:
		return "    "
	case r == '\t', IsLineBreak(r):
		return " "
	case isControl(r), isBidiControl(r):
		return Visible(r)
	}
	return string(r)
}

func Exposed(s string) string {
	var b strings.Builder
	for _, r := range s {
		if r > ' ' && r < 0x7f && r != '<' {
			b.WriteRune(r)
			continue
		}
		b.WriteString(Visible(r))
	}
	return b.String()
}

func Quote(s string) string {
	var b strings.Builder
	b.WriteByte('"')
	for _, r := range s {
		switch {
		case r == '"', r == '\\':
			b.WriteByte('\\')
			b.WriteRune(r)
		case isControl(r), IsLineBreak(r), isBidiControl(r):
			fmt.Fprintf(&b, "\\u%04x", r)
		default:
			b.WriteRune(r)
		}
	}
	b.WriteByte('"')
	return b.String()
}

func EditableRune(r rune) string {
	if r == '\t' || r == '\n' || !isControl(r) && !isBidiControl(r) && !IsLineBreak(r) {
		return string(r)
	}
	return Visible(r)
}

func Escaped(s string) string {
	var b strings.Builder
	for _, r := range s {
		b.WriteString(EditableRune(r))
	}
	return b.String()
}

func DistinctLines(sources []string) []string {
	lines := make([]string, len(sources))
	firstSource := make(map[string]string, len(sources))
	collides := make(map[string]bool, len(sources))
	for i, source := range sources {
		lines[i] = DisplayLine(source)
		key := visibleKey(lines[i])
		if first, seen := firstSource[key]; !seen {
			firstSource[key] = source
		} else if first != source {
			collides[key] = true
		}
	}
	for i, source := range sources {
		if collides[visibleKey(lines[i])] {
			lines[i] = Exposed(source)
		}
	}
	return lines
}

func visibleKey(line string) string {
	return strings.Map(func(r rune) rune {
		if lookup(r).property&defaultIgnorable != 0 {
			return -1
		}
		return r
	}, line)
}
