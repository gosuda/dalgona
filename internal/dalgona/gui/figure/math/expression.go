package math

import (
	"image"
	stdmath "math"
	"sort"
	"strconv"
	"strings"
)

var primeWords = [...]string{"", "prime", "double prime", "triple prime", "quadruple prime"}

var accentWords = map[string]string{
	"ddot":           "double dot",
	"vec":            "vector",
	"widehat":        "hat",
	"widetilde":      "tilde",
	"overrightarrow": "right arrow",
	"overleftarrow":  "left arrow",
}

var alphabetWords = map[string]string{
	"mathbf":  "bold",
	"mathit":  "italic",
	"mathsf":  "sans-serif",
	"mathtt":  "monospace",
	"mathbb":  "double-struck",
	"mathcal": "script",
}

type ExpressionNode struct {
	Parent int
	Role   string
	Label  string
	Source string
	Bounds image.Rectangle
}

type speaker struct {
	layout *Layout
	nodes  []ExpressionNode
}

func (l *Layout) Expression() []ExpressionNode {
	s := &speaker{layout: l}
	s.nodes = append(s.nodes, ExpressionNode{
		Parent: -1,
		Role:   "formula",
		Source: l.formula.Source,
		Bounds: pixelArea(0, 0, l.Width, l.Ascent+l.Descent),
	})
	label := s.list(0, l.formula.Body)
	if tag := l.formula.Tag; tag != nil {
		label = words(label, s.leaf(0, "tag", "tag "+tagLabel(tag), tag.Span))
	}
	s.nodes[0].Label = label
	return s.nodes
}

func pixelArea(left, top, right, bottom float32) image.Rectangle {
	return image.Rect(int(stdmath.Floor(float64(left))), int(stdmath.Floor(float64(top))), int(stdmath.Ceil(float64(right))), int(stdmath.Ceil(float64(bottom))))
}

func (s *speaker) open(parent int, role string, span Span) int {
	source := s.layout.formula.Source
	value := ""
	if 0 <= span.Start && span.Start <= span.End && span.End <= len(source) {
		value = source[span.Start:span.End]
	}
	s.nodes = append(s.nodes, ExpressionNode{Parent: parent, Role: role, Source: value, Bounds: s.bounds(span, parent)})
	return len(s.nodes) - 1
}

func (s *speaker) close(at int, label string) string {
	s.nodes[at].Label = label
	return label
}

func (s *speaker) leaf(parent int, role, label string, span Span) string {
	return s.close(s.open(parent, role, span), label)
}

func (s *speaker) field(parent int, role string, nodes []Node, span Span) string {
	at := s.open(parent, role, span)
	return s.close(at, s.list(at, nodes))
}

func (s *speaker) list(parent int, nodes []Node) string {
	var label strings.Builder
	for _, n := range nodes {
		spoken(&label, s.node(parent, n))
	}
	return label.String()
}

func (s *speaker) node(parent int, n Node) string {
	switch n := n.(type) {
	case *Symbol:
		return s.leaf(parent, symbolRole(n.Class), string(n.Value), n.Span)
	case *Number:
		return s.leaf(parent, "number", n.Digits, n.Span)
	case *Text:
		return s.leaf(parent, "text", n.Value, n.Span)
	case *Operator:
		return s.leaf(parent, "operator", operatorName(n), n.Span)
	case *SizedDelimiter:
		return s.delimiter(parent, n.Delimiter)
	case *Group:
		return s.list(parent, n.Body)
	case *Alphabet:
		return words(alphabetWords[n.Command], s.list(parent, n.Body))
	}
	return s.structure(parent, n)
}

func (s *speaker) structure(parent int, n Node) string {
	switch n := n.(type) {
	case *Scripts:
		return s.scripts(parent, n)
	case *Fraction:
		return s.fraction(parent, n)
	case *Radical:
		return s.radical(parent, n)
	case *Fence:
		return s.fence(parent, n)
	case *Accent:
		at := s.open(parent, "accent", n.Span)
		return s.close(at, words(s.list(at, n.Body), accentWord(n.Command)))
	case *Environment:
		return s.environment(parent, n)
	}
	return ""
}

func symbolRole(class Class) string {
	if class == ClassOrd {
		return "identifier"
	}
	return "operator"
}

func operatorName(n *Operator) string {
	if n.Value != 0 {
		return string(n.Value)
	}
	return n.Name
}

func accentWord(command string) string {
	if word, ok := accentWords[command]; ok {
		return word
	}
	return command
}

func (s *speaker) scripts(parent int, n *Scripts) string {
	at := s.open(parent, "scripts", n.Span)
	_, limits := n.Base.(*Operator)
	below, above := "subscript", "superscript"
	if limits {
		below, above = "from", "to"
	}
	label := s.base(at, n.Base)
	if n.Sub != nil {
		label = words(label, below, s.field(at, "subscript", n.Sub.Body, n.Sub.Span), scriptEnd(limits, "end subscript", n.Sub.Body))
	}
	label = words(label, primeName(n.Primes))
	if n.Sup != nil {
		label = words(label, above, s.field(at, "superscript", n.Sup.Body, n.Sup.Span), scriptEnd(limits, "end superscript", n.Sup.Body))
	}
	return s.close(at, label)
}

func (s *speaker) base(parent int, base Node) string {
	if base == nil {
		return ""
	}
	at := s.open(parent, "base", base.Range())
	return s.close(at, s.node(at, base))
}

func primeName(count int) string {
	if count < len(primeWords) {
		return primeWords[count]
	}
	return strconv.Itoa(count) + " primes"
}

func scriptEnd(limits bool, end string, body []Node) string {
	if limits || len(body) < 2 {
		return ""
	}
	return end
}

func (s *speaker) fraction(parent int, n *Fraction) string {
	role, opening, middle, closing := "fraction", "fraction", "over", "end fraction"
	if n.Command == "binom" {
		role, opening, middle, closing = "binomial", "binomial", "choose", "end binomial"
	}
	at := s.open(parent, role, n.Span)
	numerator := s.field(at, "numerator", n.Numerator, listSpan(n.Numerator))
	denominator := s.field(at, "denominator", n.Denominator, listSpan(n.Denominator))
	return s.close(at, words(opening, numerator, middle, denominator, closing))
}

func (s *speaker) radical(parent int, n *Radical) string {
	if n.Index == nil {
		at := s.open(parent, "square root", n.Span)
		return s.close(at, words("square root of", s.field(at, "radicand", n.Body, listSpan(n.Body)), "end root"))
	}
	at := s.open(parent, "root", n.Span)
	index := s.field(at, "index", n.Index.Body, n.Index.Span)
	radicand := s.field(at, "radicand", n.Body, listSpan(n.Body))
	return s.close(at, words("root with index", index, "of", radicand, "end root"))
}

func (s *speaker) fence(parent int, n *Fence) string {
	at := s.open(parent, "fenced", n.Span)
	var label strings.Builder
	spoken(&label, s.delimiter(at, n.Left))
	for i, segment := range n.Segments {
		if i > 0 && i <= len(n.Middles) {
			spoken(&label, s.delimiter(at, n.Middles[i-1]))
		}
		spoken(&label, s.list(at, segment))
	}
	spoken(&label, s.delimiter(at, n.Right))
	return s.close(at, label.String())
}

func (s *speaker) delimiter(parent int, d Delimiter) string {
	if d.Value == 0 {
		return ""
	}
	return s.leaf(parent, "delimiter", string(d.Value), d.Span)
}

func (s *speaker) environment(parent int, n *Environment) string {
	at := s.open(parent, "table", n.Span)
	var label strings.Builder
	spoken(&label, "begin "+n.Name)
	for i, row := range n.Rows {
		spoken(&label, s.row(at, i, row))
	}
	spoken(&label, "end "+n.Name)
	return s.close(at, label.String())
}

func (s *speaker) row(parent, index int, row Row) string {
	span := Span{}
	for _, cell := range row.Cells {
		span = union(span, listSpan(cell))
	}
	if row.Tag != nil {
		span = union(span, row.Tag.Span)
	}
	at := s.open(parent, "row", span)
	var label strings.Builder
	spoken(&label, "row "+strconv.Itoa(index+1))
	for _, cell := range row.Cells {
		spoken(&label, s.field(at, "cell", cell, listSpan(cell)))
	}
	if row.Tag != nil {
		spoken(&label, s.leaf(at, "tag", "tag "+tagLabel(row.Tag), row.Tag.Span))
	}
	return s.close(at, label.String())
}

func listSpan(nodes []Node) Span {
	if len(nodes) == 0 {
		return Span{}
	}
	return Span{Start: nodes[0].Range().Start, End: nodes[len(nodes)-1].Range().End}
}

func union(a, b Span) Span {
	switch {
	case a.End <= a.Start:
		return b
	case b.End <= b.Start:
		return a
	}
	return Span{Start: min(a.Start, b.Start), End: max(a.End, b.End)}
}

func spoken(label *strings.Builder, part string) {
	if part == "" {
		return
	}
	if label.Len() > 0 {
		label.WriteByte(' ')
	}
	label.WriteString(part)
}

func words(parts ...string) string {
	var label strings.Builder
	for _, part := range parts {
		spoken(&label, part)
	}
	return label.String()
}

func (s *speaker) bounds(span Span, parent int) image.Rectangle {
	regions := s.layout.regions
	first := sort.Search(len(regions), func(i int) bool { return regions[i].span.Start >= span.Start })
	left, top := float32(stdmath.Inf(1)), float32(stdmath.Inf(1))
	right, bottom := -left, -top
	for _, r := range regions[first:] {
		if r.span.Start >= span.End {
			break
		}
		if r.span.End <= span.End {
			left, top = min(left, r.left), min(top, r.top)
			right, bottom = max(right, r.left+r.width), max(bottom, r.top+r.height)
		}
	}
	if left > right {
		return s.nodes[parent].Bounds
	}
	ascent := s.layout.Ascent
	return pixelArea(left, top+ascent, right, bottom+ascent)
}
