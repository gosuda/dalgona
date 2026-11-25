package textinput

import (
	"strings"
	"unicode/utf8"
)

func Searchable(query string) bool {
	terms := 0
	for term := range strings.FieldsFuncSeq(query, IsWhitespace) {
		if utf8.RuneCountInString(term) < SearchTermScalars {
			return false
		}
		terms++
	}
	return terms > 0
}

func VisuallyBlank(s string) bool {
	for i := 0; i < len(s); {
		if length := EscapeLength(s[i:]); length > 0 {
			i += length
			continue
		}
		r, size := utf8.DecodeRuneInString(s[i:])
		if !isInvisible(r) {
			return false
		}
		i += size
	}
	return true
}

func IsWhitespace(r rune) bool {
	return lookup(r).property&whiteSpace != 0
}

func isInvisible(r rune) bool {
	return r <= 0x1f || r >= 0x7f && r <= 0x9f || lookup(r).property&(whiteSpace|defaultIgnorable) != 0
}
