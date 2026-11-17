package shellargs

import (
	"strings"
	"unicode"
)

type charKind uint8

const (
	literalChar charKind = iota
	escapedChar
	quoteChar
	spaceChar
	operatorChar
)

type shellScanner struct {
	quote   rune
	escaped bool
}

func (s *shellScanner) advance(r rune) charKind {
	if s.escaped {
		s.escaped = false
		return literalChar
	}
	if s.quote != 0 {
		return s.advanceQuoted(r)
	}
	if r == '\\' {
		s.escaped = true
		return escapedChar
	}
	if r == '\'' || r == '"' {
		s.quote = r
		return quoteChar
	}
	return unquotedKind(r)
}

func (s *shellScanner) advanceQuoted(r rune) charKind {
	if r == s.quote {
		s.quote = 0
		return quoteChar
	}
	if r == '\\' && s.quote != '\'' {
		s.escaped = true
		return escapedChar
	}
	return literalChar
}

func unquotedKind(r rune) charKind {
	switch {
	case r == '\n' || strings.ContainsRune(";|&(){}", r):
		return operatorChar
	case unicode.IsSpace(r):
		return spaceChar
	default:
		return literalChar
	}
}

func Split(command string) []string {
	var out []string
	var scanner shellScanner
	start := 0
	for i, r := range command {
		if scanner.advance(r) == operatorChar {
			out = appendPart(out, command[start:i])
			start = i + 1
		}
	}
	return appendPart(out, command[start:])
}

func appendPart(parts []string, part string) []string {
	if part = strings.TrimSpace(part); part != "" {
		return append(parts, part)
	}
	return parts
}

type word struct {
	text    strings.Builder
	started bool
}

func (w *word) write(r rune) {
	w.text.WriteRune(r)
	w.started = true
}

func (w *word) flush(tokens []string) []string {
	if !w.started {
		return tokens
	}
	tokens = append(tokens, w.text.String())
	w.text.Reset()
	w.started = false
	return tokens
}

func Tokens(s string) ([]string, bool) {
	var out []string
	var current word
	var scanner shellScanner
	for _, r := range s {
		switch scanner.advance(r) {
		case literalChar, operatorChar:
			current.write(r)
		case quoteChar:
			current.started = true
		case spaceChar:
			out = current.flush(out)
		case escapedChar:
		}
	}
	if scanner.quote != 0 || scanner.escaped {
		return nil, false
	}
	return current.flush(out), true
}

func QuoteToken(token string) string {
	if token != "" && !strings.ContainsFunc(token, needsTokenQuote) {
		return token
	}
	return "'" + strings.ReplaceAll(token, "'", `'\''`) + "'"
}

func needsTokenQuote(r rune) bool {
	return unicode.IsSpace(r) || strings.ContainsRune("'\"\\", r)
}
