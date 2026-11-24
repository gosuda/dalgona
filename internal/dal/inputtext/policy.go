package inputtext

import (
	"strings"
	"unicode/utf8"
)

const answerScalars = 2000

type Lines uint8

const (
	MultiLine Lines = iota
	SingleLine
)

type Limit struct {
	Unit Unit
	Max  int
}

type Policy struct {
	Lines Lines
	Limit Limit
}

var Answer = Policy{Lines: MultiLine, Limit: Limit{Unit: Scalars, Max: answerScalars}}

type Item struct {
	Field  string
	Policy Policy
	Text   string
}

type FieldError struct {
	Field  string
	Reason Reason
}

func (e *FieldError) Error() string { return "invalid text: " + e.Field + ": " + string(e.Reason) }

func (e *FieldError) Unwrap() error { return e.Reason }

func CheckRequest(items []Item) error {
	for _, item := range items {
		if reason := item.Policy.reject(item.Text); reason != "" {
			return &FieldError{Field: item.Field, Reason: reason}
		}
	}
	return nil
}

func (p Policy) Check(text string) error {
	if reason := p.reject(text); reason != "" {
		return reason
	}
	return nil
}

func (p Policy) reject(text string) Reason {
	if !utf8.ValidString(text) {
		return InvalidEncoding
	}
	if reason := p.rejectContent(text); reason != "" {
		return reason
	}
	if Count(text, p.Limit.Unit) > p.Limit.Max {
		return TextTooLarge
	}
	return ""
}

func (p Policy) rejectContent(text string) Reason {
	if strings.IndexByte(text, 0) >= 0 {
		return DisallowedControl
	}
	if p.Lines == SingleLine && strings.ContainsFunc(text, isLineBreak) {
		return LineBreakNotAllowed
	}
	return ""
}

func (p Policy) adapt(candidate string) string {
	if p.Lines == SingleLine || strings.IndexByte(candidate, '\r') < 0 {
		return candidate
	}
	var text strings.Builder
	text.Grow(len(candidate))
	for i := 0; i < len(candidate); i++ {
		switch {
		case candidate[i] != '\r':
			text.WriteByte(candidate[i])
		case i+1 < len(candidate) && candidate[i+1] == '\n':
		default:
			text.WriteByte('\n')
		}
	}
	return text.String()
}

func isLineBreak(r rune) bool {
	return r >= '\n' && r <= '\r' || r == '\u0085' || r == '\u2028' || r == '\u2029'
}
