package syntax

import "strings"

func importSpecs(lang, text string) []string {
	switch lang {
	case "go", "typescript":
		return singleSpec(stripQuotes(text))
	case "python":
		return singleSpec(text)
	case "java":
		return singleSpec(javaSpec(text))
	case "rust":
		return rustSpecs(text)
	}
	return nil
}

func singleSpec(spec string) []string {
	if spec == "" {
		return nil
	}
	return []string{spec}
}

func stripQuotes(text string) string {
	if len(text) < 2 {
		return text
	}
	first := text[0]
	if first != text[len(text)-1] || (first != '"' && first != '\'' && first != '`') {
		return text
	}
	return text[1 : len(text)-1]
}

func javaSpec(text string) string {
	fields := strings.Fields(text)
	if len(fields) > 0 && fields[0] == "import" {
		fields = fields[1:]
	}
	if len(fields) > 0 && fields[0] == "static" {
		fields = fields[1:]
	}
	return strings.TrimSuffix(strings.Join(fields, ""), ";")
}

func rustSpecs(text string) []string {
	body := strings.TrimSpace(trimVisibility(strings.TrimSpace(text)))
	if rest, ok := trimUseKeyword(body); ok {
		body = rest
	}
	body = strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(body), ";"))
	scan := useScanner{text: body}
	return scan.tree("", nil)
}

func trimVisibility(text string) string {
	rest, ok := strings.CutPrefix(text, "pub")
	if !ok || startsWord(rest) {
		return text
	}
	rest = strings.TrimSpace(rest)
	if strings.HasPrefix(rest, "(") {
		if _, after, found := strings.Cut(rest, ")"); found {
			rest = after
		}
	}
	return strings.TrimSpace(rest)
}

func trimUseKeyword(text string) (string, bool) {
	rest, ok := strings.CutPrefix(text, "use")
	if !ok || startsWord(rest) {
		return text, false
	}
	return rest, true
}

func startsWord(text string) bool {
	return text != "" && isWordByte(text[0])
}

func isWordByte(b byte) bool {
	return b >= 0x80 || b == '_' || b == '#' || isAlnumByte(b)
}

func isAlnumByte(b byte) bool {
	return b >= '0' && b <= '9' || b >= 'a' && b <= 'z' || b >= 'A' && b <= 'Z'
}

func isSpaceByte(b byte) bool {
	return b == ' ' || b == '\t' || b == '\n' || b == '\r'
}

type useScanner struct {
	text string
	pos  int
}

func (s *useScanner) skip() {
	for s.pos < len(s.text) && isSpaceByte(s.text[s.pos]) {
		s.pos++
	}
}

func (s *useScanner) accept(literal string) bool {
	s.skip()
	if !strings.HasPrefix(s.text[s.pos:], literal) {
		return false
	}
	s.pos += len(literal)
	return true
}

func (s *useScanner) word() string {
	s.skip()
	begin := s.pos
	for s.pos < len(s.text) && isWordByte(s.text[s.pos]) {
		s.pos++
	}
	return s.text[begin:s.pos]
}

func (s *useScanner) segment() string {
	if s.accept("*") {
		return "*"
	}
	return s.word()
}

func (s *useScanner) path() string {
	lead := s.accept("::")
	var parts []string
	for {
		part := s.segment()
		if part == "" {
			break
		}
		parts = append(parts, part)
		if !s.accept("::") {
			break
		}
	}
	spec := strings.Join(parts, "::")
	if lead {
		return "::" + spec
	}
	return spec
}

func (s *useScanner) alias() {
	saved := s.pos
	if s.word() == "as" {
		s.word()
		return
	}
	s.pos = saved
}

func (s *useScanner) tree(prefix string, out []string) []string {
	path := s.path()
	if s.accept("{") {
		return s.group(joinUse(prefix, path), out)
	}
	s.alias()
	if path == "" {
		return out
	}
	return append(out, leafUse(prefix, path))
}

func (s *useScanner) group(prefix string, out []string) []string {
	for !s.accept("}") {
		before := s.pos
		out = s.tree(prefix, out)
		s.accept(",")
		if s.pos == before {
			break
		}
	}
	return out
}

func joinUse(prefix, path string) string {
	if prefix == "" {
		return path
	}
	if path == "" {
		return prefix
	}
	return prefix + "::" + path
}

func leafUse(prefix, path string) string {
	if path == "self" && prefix != "" {
		return prefix
	}
	return joinUse(prefix, path)
}
