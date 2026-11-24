package textinput

import (
	"sort"
	"strings"
	"unicode/utf8"
)

type Reason string

const (
	InvalidEncoding     Reason = "invalid_encoding"
	TextTooLarge        Reason = "text_too_large"
	LineBreakNotAllowed Reason = "line_break_not_allowed"
	DisallowedControl   Reason = "disallowed_control"
	InvalidOffset       Reason = "invalid_offset"
)

type Error struct {
	Field  string
	Reason Reason
}

func (e *Error) Error() string { return "invalid text: " + e.Field + ": " + string(e.Reason) }

const AnswerScalars = 2000

type Field struct {
	Name       string
	SingleLine bool
	MaxScalars int
	MaxBytes   int
}

func (f Field) Check(s string) error {
	reason := f.reason(s)
	if reason == "" {
		return nil
	}
	return &Error{Field: f.Name, Reason: reason}
}

func (f Field) reason(s string) Reason {
	switch {
	case !utf8.ValidString(s):
		return InvalidEncoding
	case f.MaxBytes > 0 && len(s) > f.MaxBytes:
		return TextTooLarge
	}
	return f.scalarReason(s)
}

func (f Field) scalarReason(s string) Reason {
	scalars := 0
	for _, r := range s {
		scalars++
		switch {
		case f.MaxScalars > 0 && scalars > f.MaxScalars:
			return TextTooLarge
		case r == 0:
			return DisallowedControl
		case f.SingleLine && IsLineBreak(r):
			return LineBreakNotAllowed
		}
	}
	return ""
}

func (f Field) CheckBytes(b []byte) error {
	return f.Check(string(b))
}

func IsLineBreak(r rune) bool {
	switch r {
	case '\n', '\r', '\v', '\f', 0x85, 0x2028, 0x2029:
		return true
	}
	return false
}

func NewlinesToLF(s string) string {
	if !strings.ContainsRune(s, '\r') {
		return s
	}
	return strings.ReplaceAll(strings.ReplaceAll(s, "\r\n", "\n"), "\r", "\n")
}

func IsWhiteSpace(r rune) bool {
	i := sort.Search(len(whiteSpaceRanges), func(i int) bool { return whiteSpaceRanges[i].hi >= r })
	return i < len(whiteSpaceRanges) && whiteSpaceRanges[i].lo <= r
}

func Blank(s string) bool {
	for _, r := range s {
		if !IsWhiteSpace(r) {
			return false
		}
	}
	return true
}

const SearchTermScalars = 3

func SearchEligible(s string) bool {
	terms := strings.FieldsFunc(s, IsWhiteSpace)
	if len(terms) == 0 {
		return false
	}
	for _, term := range terms {
		if utf8.RuneCountInString(term) < SearchTermScalars {
			return false
		}
	}
	return true
}
