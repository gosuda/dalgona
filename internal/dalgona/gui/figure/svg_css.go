package figure

import (
	"cmp"
	"slices"
	"strings"
)

type declaration struct {
	name  string
	value string
}

type nesting struct {
	depth int
	quote byte
}

func (n *nesting) topLevel(c byte) bool {
	switch {
	case n.quote != 0:
		if c == n.quote {
			n.quote = 0
		}
		return false
	case c == '"' || c == '\'':
		n.quote = c
		return false
	case c == '(':
		n.depth++
		return false
	case c == ')':
		n.depth--
		return false
	}
	return n.depth == 0
}

func splitTopLevel(s string, sep byte) []string {
	var parts []string
	var scan nesting
	start := 0
	for i := 0; i < len(s); i++ {
		if scan.topLevel(s[i]) && s[i] == sep {
			parts = append(parts, s[start:i])
			start = i + 1
		}
	}
	return append(parts, s[start:])
}

func parseDeclarations(block string) ([]declaration, string) {
	var out []declaration
	for _, part := range splitTopLevel(stripComments(block), ';') {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		name, value, ok := strings.Cut(part, ":")
		if !ok {
			return nil, "style declaration " + quoted(part) + " has no value"
		}
		value = strings.TrimSpace(strings.TrimSuffix(strings.TrimSpace(value), "!important"))
		out = append(out, declaration{name: strings.TrimSpace(name), value: value})
	}
	return out, ""
}

func quoted(s string) string {
	if len(s) > 80 {
		s = s[:80] + "..."
	}
	return `"` + s + `"`
}

func stripComments(s string) string {
	for {
		i := strings.Index(s, "/*")
		if i < 0 {
			return s
		}
		j := strings.Index(s[i+2:], "*/")
		if j < 0 {
			return s[:i]
		}
		s = s[:i] + " " + s[i+2+j+2:]
	}
}

type compound struct {
	tag     string
	classes []string
}

type selector struct {
	parts       []compound
	specificity int
}

type cssRule struct {
	selectors []selector
	decls     []declaration
	order     int
}

var droppedAtRules = map[string]bool{"import": true, "font-face": true}

func parseSheet(text string, order *int) ([]cssRule, string) {
	text = stripComments(text)
	var rules []cssRule
	for {
		text = strings.TrimSpace(text)
		if text == "" {
			return rules, ""
		}
		if strings.HasPrefix(text, "@") {
			rest, problem := skipAtRule(text)
			if problem != "" {
				return nil, problem
			}
			text = rest
			continue
		}
		rule, rest, problem := parseRule(text, order)
		if problem != "" {
			return nil, problem
		}
		rules = append(rules, rule)
		text = rest
	}
}

func skipAtRule(text string) (string, string) {
	name := text[1:strings.IndexAny(text+" ", " \t\n{;(")]
	if !droppedAtRules[name] {
		return "", "style sheet at-rule @" + name + " is outside the engine style profile"
	}
	stop := topLevelIndex(text)
	if stop >= 0 && text[stop] == ';' {
		return text[stop+1:], ""
	}
	end := matchingBrace(text, stop)
	if end < 0 {
		return "", "style sheet at-rule @" + name + " is not closed"
	}
	return text[end+1:], ""
}

func topLevelIndex(text string) int {
	var scan nesting
	for i := 0; i < len(text); i++ {
		if scan.topLevel(text[i]) && (text[i] == ';' || text[i] == '{') {
			return i
		}
	}
	return -1
}

func matchingBrace(text string, open int) int {
	if open < 0 {
		return -1
	}
	depth := 0
	for i := open; i < len(text); i++ {
		switch text[i] {
		case '{':
			depth++
		case '}':
			depth--
			if depth == 0 {
				return i
			}
		}
	}
	return -1
}

func parseRule(text string, order *int) (cssRule, string, string) {
	open := strings.IndexByte(text, '{')
	end := matchingBrace(text, open)
	if end < 0 {
		return cssRule{}, "", "style sheet rule is not closed"
	}
	decls, problem := parseDeclarations(text[open+1 : end])
	if problem != "" {
		return cssRule{}, "", problem
	}
	var sels []selector
	for _, raw := range strings.Split(text[:open], ",") {
		sel, ok := parseSelector(strings.TrimSpace(raw))
		if !ok {
			return cssRule{}, "", "style selector " + quoted(raw) + " is outside the engine style profile"
		}
		sels = append(sels, sel)
	}
	*order++
	return cssRule{selectors: sels, decls: decls, order: *order}, text[end+1:], ""
}

func parseSelector(s string) (selector, bool) {
	fields := strings.Fields(s)
	if len(fields) == 0 || len(fields) > 2 {
		return selector{}, false
	}
	var sel selector
	for _, f := range fields {
		c, ok := parseCompound(f)
		if !ok {
			return selector{}, false
		}
		sel.parts = append(sel.parts, c)
		sel.specificity += 256 * len(c.classes)
		if c.tag != "" {
			sel.specificity++
		}
	}
	return sel, true
}

func parseCompound(s string) (compound, bool) {
	segments := strings.Split(s, ".")
	c := compound{tag: segments[0]}
	if !simpleIdentifier(c.tag, true) {
		return compound{}, false
	}
	for _, class := range segments[1:] {
		if !simpleIdentifier(class, false) {
			return compound{}, false
		}
		c.classes = append(c.classes, class)
	}
	return c, c.tag != "" || len(c.classes) > 0
}

func simpleIdentifier(s string, allowEmpty bool) bool {
	if s == "" {
		return allowEmpty
	}
	for i := 0; i < len(s); i++ {
		if !identifierByte(s[i]) {
			return false
		}
	}
	return true
}

func identifierByte(c byte) bool {
	return c == '-' || c == '_' || isDigit(c) || (c|0x20 >= 'a' && c|0x20 <= 'z')
}

func (c compound) matches(n *svgNode) bool {
	if c.tag != "" && c.tag != n.name {
		return false
	}
	for _, want := range c.classes {
		if !hasClass(n, want) {
			return false
		}
	}
	return true
}

func hasClass(n *svgNode, class string) bool {
	for _, have := range n.classes {
		if have == class {
			return true
		}
	}
	return false
}

func (s selector) matches(n *svgNode) bool {
	last := len(s.parts) - 1
	if !s.parts[last].matches(n) {
		return false
	}
	if last == 0 {
		return true
	}
	for a := n.parent; a != nil; a = a.parent {
		if s.parts[0].matches(a) {
			return true
		}
	}
	return false
}

type cssHit struct {
	specificity, order int
	decls              []declaration
}

func (r cssRule) specificity(n *svgNode) int {
	best := -1
	for _, s := range r.selectors {
		if s.matches(n) {
			best = max(best, s.specificity)
		}
	}
	return best
}

func matchingDeclarations(rules []cssRule, n *svgNode) []declaration {
	var hits []cssHit
	for _, r := range rules {
		if best := r.specificity(n); best >= 0 {
			hits = append(hits, cssHit{specificity: best, order: r.order, decls: r.decls})
		}
	}
	slices.SortStableFunc(hits, func(a, b cssHit) int {
		if c := cmp.Compare(a.specificity, b.specificity); c != 0 {
			return c
		}
		return cmp.Compare(a.order, b.order)
	})
	var out []declaration
	for _, h := range hits {
		out = append(out, h.decls...)
	}
	return out
}
