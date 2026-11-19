package tui

import (
	"strings"
	"unicode/utf8"

	"golang.org/x/text/unicode/norm"
)

func (s *screenState) nfc(text string) string {
	if norm.NFC.IsNormalString(text) {
		return text
	}
	return norm.NFC.String(text)
}

const tabStop = 8

func (s *screenState) expand(text string) string {
	if strings.IndexByte(text, '\t') < 0 {
		return text
	}
	var out strings.Builder
	out.Grow(len(text) + tabStop)
	col := 0
	for text != "" {
		i := strings.IndexAny(text, "\t\n")
		if i < 0 {
			out.WriteString(text)
			break
		}
		col += s.method.StringWidth(s.nfc(text[:i]))
		out.WriteString(text[:i])
		if text[i] == '\n' {
			out.WriteByte('\n')
			col = 0
		} else {
			pad := tabStop - col%tabStop
			out.WriteString(strings.Repeat(" ", pad))
			col += pad
		}
		text = text[i+1:]
	}
	return out.String()
}

func (s *screenState) cells(text string) int {
	return s.method.StringWidth(s.nfc(s.expand(text)))
}

func (s *screenState) fit(text string, n int) string {
	nfc := s.nfc(s.expand(plainPrefix(text, max(n, 0)*8+64)))
	if len(nfc) <= n {
		return nfc
	}
	if n < 3 {
		return s.method.Truncate(nfc, n, "")
	}
	return s.method.Truncate(nfc, n, "...")
}

func plainPrefix(text string, bound int) string {
	if len(text) <= bound || strings.IndexByte(text, '\x1b') >= 0 {
		return text
	}
	for bound > 0 && !utf8.RuneStart(text[bound]) {
		bound--
	}
	return text[:bound]
}

func (s *screenState) wrap(text string, n int) []string {
	return strings.Split(s.method.Wrap(s.nfc(s.expand(text)), n, ""), "\n")
}
