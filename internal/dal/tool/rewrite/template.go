package rewrite

import (
	"fmt"
	"strings"
)

type part struct {
	text    string
	capture string
}

type template []part

func parseTemplate(text string) (template, error) {
	var out template
	var literal strings.Builder
	flush := func() {
		if literal.Len() > 0 {
			out = append(out, part{text: literal.String()})
			literal.Reset()
		}
	}
	for i := 0; i < len(text); {
		if text[i] != '@' {
			literal.WriteByte(text[i])
			i++
			continue
		}
		if i+1 < len(text) && text[i+1] == '@' {
			literal.WriteByte('@')
			i += 2
			continue
		}
		name, next, ok := captureRef(text, i)
		if !ok {
			return nil, fmt.Errorf("rewrite: replace has a lone @ at byte %d; write @@ for a literal @", i)
		}
		flush()
		out = append(out, part{capture: name})
		i = next
	}
	flush()
	return out, nil
}

func captureRef(text string, at int) (string, int, bool) {
	if at+1 < len(text) && text[at+1] == '{' {
		return bracedRef(text, at)
	}
	return plainRef(text, at)
}

func bracedRef(text string, at int) (string, int, bool) {
	end := at + 2
	for end < len(text) && isCaptureByte(text[end], true) {
		end++
	}
	if end == at+2 || end >= len(text) || text[end] != '}' {
		return "", 0, false
	}
	return text[at+2 : end], end + 1, true
}

func plainRef(text string, at int) (string, int, bool) {
	end := at + 1
	for end < len(text) && isCaptureByte(text[end], false) {
		end++
	}
	if end == at+1 {
		return "", 0, false
	}
	return text[at+1 : end], end, true
}

func isCaptureByte(b byte, dotted bool) bool {
	switch {
	case b >= 'a' && b <= 'z', b >= 'A' && b <= 'Z', b >= '0' && b <= '9', b == '_':
		return true
	default:
		return dotted && b == '.'
	}
}

func (t template) captures() []string {
	var names []string
	for _, p := range t {
		if p.capture != "" {
			names = append(names, p.capture)
		}
	}
	return names
}

func expand(t template, src []byte, g group) ([]byte, error) {
	var out []byte
	for _, p := range t {
		if p.capture == "" {
			out = append(out, p.text...)
			continue
		}
		s, ok := g.spans[p.capture]
		if !ok {
			return nil, fmt.Errorf("rewrite: @%s is not in the match at line %d", p.capture, g.line)
		}
		out = append(out, src[s.start:s.end]...)
	}
	return out, nil
}
