package patch

import (
	"strconv"
	"strings"
)

type textMatch struct {
	start       int
	end         int
	prefixStart int
}

type viewLine struct {
	start      int
	contentEnd int
	end        int
}

func findTextMatches(view, old, context string, atEOF bool) []textMatch {
	normalized := strings.ReplaceAll(old, "\r\n", "\n")
	matches := exactMatches(view, normalized)
	if len(matches) == 0 && strings.Contains(normalized, "\n") {
		matches = numberedMatches(view, normalized)
	}
	if len(matches) == 0 {
		matches = canonicalMatches(view, normalized, 0)
	}
	return selectNonOverlapping(matches, view, context, atEOF)
}

func selectNonOverlapping(matches []textMatch, view, context string, atEOF bool) []textMatch {
	out := make([]textMatch, 0, len(matches))
	for _, match := range matches {
		if len(out) > 0 && match.start < out[len(out)-1].end {
			continue
		}
		if (!atEOF || match.end == len(view)) && strings.Contains(view[:match.start], context) {
			out = append(out, match)
		}
	}
	return out
}

func numberedMatches(view, normalized string) []textMatch {
	start, stripped, ok := stripNumberedPrefixes(normalized)
	if !ok {
		return nil
	}
	matches := exactMatches(view, stripped)
	if len(matches) == 0 {
		return canonicalMatches(view, stripped, start)
	}
	for i := range matches {
		matches[i].prefixStart = start
	}
	return matches
}

func exactMatches(view, needle string) []textMatch {
	if needle == "" {
		return nil
	}
	var out []textMatch
	for offset := 0; offset <= len(view)-len(needle); {
		at := strings.Index(view[offset:], needle)
		if at < 0 {
			break
		}
		start := offset + at
		out = append(out, textMatch{start: start, end: start + len(needle)})
		offset = start + 1
	}
	return out
}

func viewLines(text string) []viewLine {
	var out []viewLine
	start := 0
	for i, char := range []byte(text) {
		if char != '\n' {
			continue
		}
		out = append(out, viewLine{start: start, contentEnd: i, end: i + 1})
		start = i + 1
	}
	if start < len(text) {
		out = append(out, viewLine{start: start, contentEnd: len(text), end: len(text)})
	}
	return out
}

func canonicalMatches(view, needle string, prefix int) []textMatch {
	haystack, needles := viewLines(view), viewLines(needle)
	if len(needles) == 0 || len(needles) > len(haystack) {
		return nil
	}
	needleKeys := make([]string, len(needles))
	for i, line := range needles {
		needleKeys[i] = canonicalLine(needle[line.start:line.contentEnd])
	}
	finalNewline := needles[len(needles)-1].end > needles[len(needles)-1].contentEnd
	var out []textMatch
	for start := 0; start+len(needles) <= len(haystack); start++ {
		if !canonicalLinesAt(haystack, needleKeys, view, start) {
			continue
		}
		last := haystack[start+len(needles)-1]
		if finalNewline && last.end == last.contentEnd {
			continue
		}
		end := last.contentEnd
		if finalNewline {
			end = last.end
		}
		out = append(out, textMatch{start: haystack[start].start, end: end, prefixStart: prefix})
	}
	return out
}

func canonicalLinesAt(haystack []viewLine, needles []string, view string, start int) bool {
	for i, expected := range needles {
		line := haystack[start+i]
		if canonicalLine(view[line.start:line.contentEnd]) != expected {
			return false
		}
	}
	return true
}

func canonicalLine(line string) string {
	var out strings.Builder
	out.Grow(len(line))
	for _, char := range line {
		switch char {
		case '\u2018', '\u2019', '\u201b', '\u2032':
			out.WriteByte('\'')
		case '\u201c', '\u201d', '\u201f', '\u2033':
			out.WriteByte('"')
		case '\u2010', '\u2011', '\u2012', '\u2013', '\u2014', '\u2015', '\u2212':
			out.WriteByte('-')
		case '\u00a0', '\u2007', '\u202f':
			out.WriteByte(' ')
		case '\u2026':
			out.WriteString("...")
		default:
			out.WriteRune(char)
		}
	}
	return strings.TrimRight(out.String(), " \t\r")
}

func stripNumberedPrefixes(value string) (int, string, bool) {
	lines := viewLines(value)
	if len(lines) < 2 {
		return 0, "", false
	}
	var out strings.Builder
	first, expected := 0, 0
	for i, span := range lines {
		line := value[span.start:span.contentEnd]
		numberText, body, ok := strings.Cut(line, ":")
		if !ok {
			return 0, "", false
		}
		number, err := strconv.Atoi(numberText)
		if err != nil || i > 0 && number != expected {
			return 0, "", false
		}
		if i == 0 {
			first = number
		}
		expected = number + 1
		out.WriteString(body)
		if span.end > span.contentEnd {
			out.WriteByte('\n')
		}
	}
	return first, out.String(), first > 0
}
