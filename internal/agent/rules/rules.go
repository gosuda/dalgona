package rules

import (
	"regexp"
	"slices"
	"strings"
	"unicode"
)

var risky = []struct {
	name string
	re   *regexp.Regexp
}{
	{"substitution", regexp.MustCompile(`\$\(|<\(|>\(|` + "`" + `|\\\r?\n`)},
	{"shell-wrapper", regexp.MustCompile(`^(\S*['"\\=$]|[-0-9])`)},
	{"privilege", regexp.MustCompile(`^(sudo|doas|su)\b`)},
	{"nested-shell", regexp.MustCompile(`^(bash|sh|zsh|fish|eval|xargs)\b`)},
	{"inline-interpreter", regexp.MustCompile(`^(node|bun|python3?)\s+(-\S+\s+)*(-c|-e|-p|--eval)\b`)},
	{"recursive-delete", regexp.MustCompile(`^rm\s+(-\S*[rR]\S*\s+)+`)},
	{"git-destructive", regexp.MustCompile(`^git\b.*\b(push|reset\s+--hard|clean\s+-\S*[fd]|branch\s+-D)\b`)},
	{"publish", regexp.MustCompile(`^(npm|pnpm|yarn|bun)\s+publish\b`)},
	{"deploy", regexp.MustCompile(`^(wrangler|vercel|fly|flyctl)\s+deploy\b|^terraform\s+(apply|destroy)\b|^kubectl\s+(delete|apply)\b`)},
	{"credentials", regexp.MustCompile(`(?i)\.env\b|\.ssh\b|\.aws\b|\.npmrc\b|\.netrc\b|credentials`)},
}

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

var (
	shellKeywordPrefix   = regexp.MustCompile(`^(?:if|then|elif|else|fi|while|until|do|done|for|in|case|esac|!)\s+`)
	commandWrapperPrefix = regexp.MustCompile(`^(?:command|builtin|exec|env|nohup|time|timeout|nice|watch|npx|bunx|pnpx)\s+`)
	assignmentPrefix     = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*=[^\s'"\\$]*\s+`)
	executablePathPrefix = regexp.MustCompile(`^[^\s'"\\$]*/`)
)

func normalize(part string) string {
	current := strings.TrimSpace(part)
	for {
		next := strings.TrimSpace(current)
		next = shellKeywordPrefix.ReplaceAllString(next, "")
		next = commandWrapperPrefix.ReplaceAllString(next, "")
		next = assignmentPrefix.ReplaceAllString(next, "")
		next = strings.TrimPrefix(next, `\`)
		next = executablePathPrefix.ReplaceAllString(next, "")
		if next == current {
			return current
		}
		current = next
	}
}

func Check(command string) (string, string, bool) {
	if risky[0].re.MatchString(command) {
		return "substitution", command, true
	}
	for _, part := range Split(command) {
		normal := normalize(part)
		for _, rule := range risky[1:] {
			if rule.re.MatchString(normal) || rule.name == "credentials" && rule.re.MatchString(part) {
				return rule.name, part, true
			}
		}
	}
	return "", "", false
}

func GrantPrefix(command string) (string, bool) {
	parts := Split(command)
	if len(parts) != 1 {
		return "", false
	}
	tokens := tokenize(parts[0])
	if len(tokens) == 0 {
		return "", false
	}
	prefix := quoteToken(tokens[0])
	if len(tokens) > 1 && !strings.HasPrefix(tokens[1], "-") {
		prefix += " " + quoteToken(tokens[1])
	}
	return prefix, true
}

func quoteToken(token string) string {
	if token != "" && !strings.ContainsFunc(token, needsTokenQuote) {
		return token
	}
	return "'" + strings.ReplaceAll(token, "'", `'\''`) + "'"
}

func needsTokenQuote(r rune) bool {
	return unicode.IsSpace(r) || strings.ContainsRune("'\"\\", r)
}

func tokenize(s string) []string {
	var out []string
	var token strings.Builder
	var scanner shellScanner
	for _, r := range s {
		switch scanner.advance(r) {
		case literalChar, operatorChar:
			token.WriteRune(r)
		case spaceChar:
			out = flushToken(out, &token)
		case escapedChar, quoteChar:
		}
	}
	return flushToken(out, &token)
}

func flushToken(tokens []string, token *strings.Builder) []string {
	if token.Len() == 0 {
		return tokens
	}
	tokens = append(tokens, token.String())
	token.Reset()
	return tokens
}

func MatchesGrant(command, prefix string) bool {
	parts := Split(command)
	if len(parts) != 1 {
		return false
	}
	granted := tokenize(prefix)
	candidate := tokenize(parts[0])
	if len(granted) == 0 || len(candidate) < len(granted) {
		return false
	}
	return slices.Equal(candidate[:len(granted)], granted)
}
