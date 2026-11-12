package tui

import (
	"strings"

	"golang.org/x/text/unicode/norm"
)

func (s *screenState) cells(text string) int {
	return s.method.StringWidth(norm.NFC.String(text))
}

func (s *screenState) fit(text string, n int) string {
	nfc := norm.NFC.String(text)
	if s.method.StringWidth(nfc) <= n {
		return nfc
	}
	if n < 3 {
		return s.method.Truncate(nfc, n, "")
	}
	return s.method.Truncate(nfc, n, "...")
}

func (s *screenState) wrap(text string, n int) []string {
	return strings.Split(s.method.Wrap(norm.NFC.String(text), n, ""), "\n")
}
