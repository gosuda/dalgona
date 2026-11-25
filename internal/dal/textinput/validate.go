package textinput

import (
	"strings"
	"unicode/utf16"
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
	if reason := checkText(s, f.SingleLine, f.MaxScalars, f.MaxBytes, 0); reason != "" {
		return &Error{Field: f.Name, Reason: reason}
	}
	return nil
}

func checkText(text string, singleLine bool, maxScalars, maxBytes, maxUTF16 int) Reason {
	if !utf8.ValidString(text) {
		return InvalidEncoding
	}
	if reason := contentReason(text, singleLine); reason != "" {
		return reason
	}
	return quotaReason(text, maxScalars, maxBytes, maxUTF16)
}

func contentReason(text string, singleLine bool) Reason {
	switch {
	case strings.IndexByte(text, 0) >= 0:
		return DisallowedControl
	case singleLine && strings.ContainsFunc(text, IsLineBreak):
		return LineBreakNotAllowed
	}
	return ""
}

func quotaReason(text string, maxScalars, maxBytes, maxUTF16 int) Reason {
	if maxBytes > 0 && len(text) > maxBytes {
		return TextTooLarge
	}
	if overQuota(text, maxScalars, maxUTF16) {
		return TextTooLarge
	}
	return ""
}

func overQuota(text string, maxScalars, maxUTF16 int) bool {
	if maxScalars <= 0 && maxUTF16 <= 0 {
		return false
	}
	scalars, units := 0, 0
	for _, r := range text {
		scalars++
		if maxScalars > 0 && scalars > maxScalars {
			return true
		}
		units += utf16.RuneLen(r)
		if maxUTF16 > 0 && units > maxUTF16 {
			return true
		}
	}
	return false
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
	return lookup(r).property&whiteSpace != 0
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
