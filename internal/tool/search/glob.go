package search

import (
	"errors"
	"fmt"
	"regexp"
	"strings"
)

type glob struct {
	re *regexp.Regexp
}

func compileGlob(pattern string, basename bool) (glob, error) {
	var out strings.Builder
	out.WriteByte('^')
	if basename {
		out.WriteString(`(?:.*/)?`)
	}
	for i := 0; i < len(pattern); {
		next, err := translateGlob(&out, pattern, i)
		if err != nil {
			return glob{}, fmt.Errorf("search: invalid glob %s: %w", pattern, err)
		}
		i = next
	}
	out.WriteByte('$')
	re, err := regexp.Compile(out.String())
	if err != nil {
		return glob{}, fmt.Errorf("search: invalid glob %s: %w", pattern, err)
	}
	return glob{re: re}, nil
}

func translateGlob(out *strings.Builder, pattern string, i int) (int, error) {
	switch pattern[i] {
	case '*':
		return translateStar(out, pattern, i), nil
	case '?':
		out.WriteString(`[^/]`)
		return i + 1, nil
	case '[':
		return translateClass(out, pattern, i)
	case '\\':
		if i+1 >= len(pattern) {
			return 0, errors.New("trailing escape")
		}
		out.WriteString(regexp.QuoteMeta(pattern[i+1 : i+2]))
		return i + 2, nil
	default:
		out.WriteString(regexp.QuoteMeta(pattern[i : i+1]))
		return i + 1, nil
	}
}

func translateStar(out *strings.Builder, pattern string, i int) int {
	rest := pattern[i:]
	switch {
	case strings.HasPrefix(rest, "**/"):
		out.WriteString(`(?:.*/)?`)
		return i + 3
	case strings.HasPrefix(rest, "**"):
		out.WriteString(".*")
		return i + 2
	default:
		out.WriteString(`[^/]*`)
		return i + 1
	}
}

func translateClass(out *strings.Builder, pattern string, i int) (int, error) {
	end := strings.IndexByte(pattern[i+1:], ']')
	if end < 0 {
		return 0, errors.New("missing ]")
	}
	end += i + 1
	body := pattern[i+1 : end]
	if rest, ok := strings.CutPrefix(body, "!"); ok {
		body = "^" + rest
	}
	out.WriteByte('[')
	out.WriteString(body)
	out.WriteByte(']')
	return end + 1, nil
}

func (g glob) match(path string) bool {
	return g.re.MatchString(strings.ReplaceAll(path, "\\", "/"))
}
