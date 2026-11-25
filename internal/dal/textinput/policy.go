package textinput

import "strings"

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

var Answer = Policy{Lines: MultiLine, Limit: Limit{Unit: Scalars, Max: AnswerScalars}}

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
	limit := p.Limit
	scalars, octets, units := 0, 0, 0
	switch limit.Unit {
	case Scalars:
		scalars = limit.Max
	case Bytes:
		octets = limit.Max
	case UTF16Units:
		units = limit.Max
	}
	return checkText(text, p.Lines == SingleLine, scalars, octets, units)
}

func (p Policy) rejectContent(text string) Reason {
	return contentReason(text, p.Lines == SingleLine)
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
