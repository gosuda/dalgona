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

func (s *screenState) cells(text string) int {
	return s.method.StringWidth(s.nfc(text))
}

func (s *screenState) fit(text string, n int) string {
	nfc := s.nfc(plainPrefix(text, max(n, 0)*8+64))
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
	return strings.Split(s.method.Wrap(s.nfc(text), n, ""), "\n")
}
