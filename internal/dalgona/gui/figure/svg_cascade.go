package figure

import (
	"maps"
	"strings"
)

type Profile uint8

const (
	ProfileSVG Profile = iota
	ProfileMermaid
	ProfileD2
	ProfileNomnoml
)

func (p Profile) engine() bool { return p != ProfileSVG }

var nonInheritedCopies = map[string]func(s, parent *style){
	"opacity":        func(s, p *style) { s.opacity = p.opacity },
	"display":        func(s, p *style) { s.display = p.display },
	"clip-path":      func(s, p *style) { s.clipPath = p.clipPath },
	"mask":           func(s, p *style) { s.mask = p.mask },
	"filter":         func(s, p *style) { s.filter = p.filter },
	"stop-color":     func(s, p *style) { s.stopColor = p.stopColor },
	"stop-opacity":   func(s, p *style) { s.stopOpacity = p.stopOpacity },
	"flood-color":    func(s, p *style) { s.floodColor = p.floodColor },
	"flood-opacity":  func(s, p *style) { s.floodOpacity = p.floodOpacity },
	"overflow":       func(s, p *style) { s.overflow = p.overflow },
	"background":     func(s, p *style) { s.background = p.background },
	"mix-blend-mode": func(s, p *style) { s.multiply = p.multiply },
	"alignment-baseline": func(s, p *style) {
		s.alignBaseline, s.alignSet = p.alignBaseline, p.alignSet
	},
}

var leadingProperties = []string{"color", "font-size", "font-family", "font-weight"}

func (d *decoder) declarations(n *svgNode) ([]declaration, error) {
	var out []declaration
	for _, a := range n.attrs {
		if a.space != "" || a.name == "transform" {
			continue
		}
		if _, ok := properties[a.name]; ok {
			out = append(out, declaration{name: a.name, value: strings.TrimSpace(a.value)})
		}
	}
	out = append(out, matchingDeclarations(d.sheet, n)...)
	if inline, ok := n.attr("style"); ok {
		decls, problem := parseDeclarations(inline)
		if problem != "" {
			return nil, failAt(FailureInvalid, n.span, "%s", problem)
		}
		out = append(out, decls...)
	}
	return out, nil
}

func (d *decoder) computeStyle(n *svgNode, parent *style) (*style, error) {
	decls, err := d.declarations(n)
	if err != nil {
		return nil, err
	}
	s := parent.inherit()
	for _, decl := range decls {
		if !strings.HasPrefix(decl.name, "--") {
			continue
		}
		if err := d.customProperty(n, s, decl); err != nil {
			return nil, err
		}
	}
	for _, decl := range cascadeOrder(decls) {
		if err := d.applyProperty(n, s, parent, decl.name, decl.value); err != nil {
			return nil, err
		}
	}
	return s, nil
}

func cascadeOrder(decls []declaration) []declaration {
	final := map[string]string{}
	var order []string
	for _, decl := range decls {
		if strings.HasPrefix(decl.name, "--") {
			continue
		}
		if _, seen := final[decl.name]; !seen {
			order = append(order, decl.name)
		}
		final[decl.name] = decl.value
	}
	out := make([]declaration, 0, len(order))
	for _, name := range leadingProperties {
		if v, ok := final[name]; ok {
			out = append(out, declaration{name: name, value: v})
			delete(final, name)
		}
	}
	for _, name := range order {
		if v, ok := final[name]; ok {
			out = append(out, declaration{name: name, value: v})
		}
	}
	return out
}

func (d *decoder) customProperty(n *svgNode, s *style, decl declaration) error {
	if !d.opts.Profile.engine() {
		return failAt(FailureInvalid, n.span, "custom property %s is outside the static SVG profile; write the value directly", decl.name)
	}
	if len(s.custom) >= 256 {
		return failAt(FailureLimit, n.span, "element declares more than 256 custom properties")
	}
	s.custom = maps.Clone(s.custom)
	if s.custom == nil {
		s.custom = map[string]string{}
	}
	s.custom[decl.name] = decl.value
	return nil
}

func (d *decoder) applyProperty(n *svgNode, s, parent *style, name, value string) error {
	prop, ok := properties[name]
	if !ok {
		return failAt(FailureInvalid, n.span, "style property %q on <%s> is outside the static SVG profile; remove it", name, n.name)
	}
	if prop.parse == nil {
		return nil
	}
	resolved, err := d.substitute(n, s, value, 0)
	if err != nil {
		return err
	}
	resolved = strings.TrimSpace(resolved)
	if resolved == "inherit" {
		if copyParent, ok := nonInheritedCopies[name]; ok {
			copyParent(s, parent)
		}
		return nil
	}
	if !prop.parse(&d.ctx, s, parent, resolved) {
		return failAt(FailureInvalid, n.span, "style property %s on <%s> has value %s that the static SVG profile cannot draw", name, n.name, quoted(resolved))
	}
	return nil
}

const substitutionDepth = 16

func (d *decoder) substitute(n *svgNode, s *style, value string, depth int) (string, error) {
	i := strings.Index(value, "var(")
	if i < 0 {
		return value, nil
	}
	if !d.opts.Profile.engine() {
		return "", failAt(FailureInvalid, n.span, "style variables are outside the static SVG profile; write the value directly")
	}
	if depth >= substitutionDepth {
		return "", failAt(FailureLimit, n.span, "style variables nest deeper than %d levels", substitutionDepth)
	}
	end := matchingParen(value, i+3)
	if end < 0 {
		return "", failAt(FailureInvalid, n.span, "style variable reference is not closed")
	}
	name, fallback, hasFallback := strings.Cut(value[i+4:end], ",")
	replacement, ok := s.custom[strings.TrimSpace(name)]
	if !ok {
		if !hasFallback {
			return "", failAt(FailureInvalid, n.span, "style variable %s is not defined", strings.TrimSpace(name))
		}
		replacement = fallback
	}
	expanded, err := d.substitute(n, s, replacement, depth+1)
	if err != nil {
		return "", err
	}
	return d.substitute(n, s, value[:i]+strings.TrimSpace(expanded)+value[end+1:], depth+1)
}

func matchingParen(s string, open int) int {
	depth := 0
	for i := open; i < len(s); i++ {
		switch s[i] {
		case '(':
			depth++
		case ')':
			depth--
			if depth == 0 {
				return i
			}
		}
	}
	return -1
}
