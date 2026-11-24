package document

import (
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/yuin/goldmark/ast"
	east "github.com/yuin/goldmark/extension/ast"
	"github.com/yuin/goldmark/util"
)

type inlineContext struct {
	style Style
	link  int
	depth int
}

func (b *builder) inlineProjection(n ast.Node) Projection {
	p := &projector{source: b.source}
	if b.inline(p, n, inlineContext{}) {
		return p.projection()
	}
	start := b.nodeStart(n)
	b.diagnose(inlineNestingDiagnostic(Span{Start: start, End: start}))
	b.links, b.task = nil, TaskNone
	if n.Type() == ast.TypeBlock {
		return b.lineProjection(n.Lines(), 0)
	}
	return Projection{}
}

func (b *builder) inline(p *projector, n ast.Node, c inlineContext) bool {
	if c.depth > MaxNesting {
		return false
	}
	for child := n.FirstChild(); child != nil; child = child.NextSibling() {
		if !b.inlineNode(p, child, c) {
			return false
		}
	}
	return true
}

func (b *builder) inlineNode(p *projector, n ast.Node, c inlineContext) bool {
	nested := inlineContext{style: c.style, link: c.link, depth: c.depth + 1}
	b.styleNested(p, n, c, &nested)
	b.linkNested(p, n, c, &nested)
	return b.inline(p, n, nested)
}

func (b *builder) styleNested(p *projector, n ast.Node, c inlineContext, nested *inlineContext) {
	switch n := n.(type) {
	case *ast.Text:
		b.text(p, n, c)
	case *ast.String:
		p.add(p.cursor(), string(n.Value), Generated, c.style, c.link)
	case *ast.CodeSpan:
		nested.style |= InlineCode
	case *ast.Emphasis:
		nested.style |= emphasisStyle(n.Level)
	case *east.Strikethrough:
		nested.style |= Strikethrough
	case *east.TaskCheckBox:
		b.task = taskState(n.IsChecked)
	}
}

func (b *builder) linkNested(p *projector, n ast.Node, c inlineContext, nested *inlineContext) {
	switch n := n.(type) {
	case *ast.Link:
		b.links = append(b.links, Link{Destination: string(n.Destination), Title: string(n.Title), Reference: referenceLabel(n.Reference)})
		nested.style, nested.link = nested.style|LinkText, len(b.links)
	case *ast.AutoLink:
		b.autoLink(p, n, c)
	case *ast.Image:
		b.image(p, n, c)
		nested.style |= ImageText
	case *ast.RawHTML:
		b.rawHTML(p, n, c)
	case *mathNode:
		b.mathRuns(p, n, c.style|InlineMath, c.link)
	}
}

func emphasisStyle(level int) Style {
	if level >= 2 {
		return Strong
	}
	return Emphasis
}

func taskState(checked bool) Task {
	if checked {
		return TaskDone
	}
	return TaskOpen
}

func referenceLabel(reference *ast.ReferenceLink) string {
	if reference == nil {
		return ""
	}
	return string(reference.Value)
}

func (p *projector) cursor() Span {
	if n := len(p.runs); n > 0 {
		end := p.runs[n-1].Source.End
		return Span{Start: end, End: end}
	}
	return Span{}
}

func (b *builder) text(p *projector, n *ast.Text, c inlineContext) {
	segment := n.Segment
	b.padding(p, segment, c.style)
	span := Span{Start: segment.Start, End: segment.Stop}
	switch {
	case c.style&InlineCode != 0:
		b.codeText(p, span, c)
	case n.IsRaw():
		p.verbatim(span, c.style, c.link)
	default:
		b.decodeText(p, span, c)
	}
	switch {
	case n.HardLineBreak():
		p.add(b.lineEnding(span.End), "\n", Break, c.style, c.link)
	case n.SoftLineBreak():
		p.add(b.lineEnding(span.End), " ", Break, c.style, c.link)
	}
}

func (b *builder) lineEnding(from int) Span {
	end := strings.IndexByte(b.source[from:], '\n')
	if end < 0 {
		return Span{Start: from, End: from}
	}
	return Span{Start: from, End: from + end + 1}
}

func (b *builder) codeText(p *projector, span Span, c inlineContext) {
	start := span.Start
	for i := span.Start; i < span.End; i++ {
		if b.source[i] != '\n' && b.source[i] != '\r' {
			continue
		}
		end := i + 1
		if b.source[i] == '\r' && end < span.End && b.source[end] == '\n' {
			end++
		}
		p.verbatim(Span{Start: start, End: i}, c.style, c.link)
		p.add(Span{Start: i, End: end}, " ", Break, c.style, c.link)
		start = end
		i = end - 1
	}
	p.verbatim(Span{Start: start, End: span.End}, c.style, c.link)
}

func (b *builder) decodeText(p *projector, span Span, c inlineContext) {
	start := span.Start
	for i := span.Start; i < span.End; {
		width, decoded := decodeAt(b.source[i:span.End])
		if width == 0 {
			i++
			continue
		}
		p.verbatim(Span{Start: start, End: i}, c.style, c.link)
		p.add(Span{Start: i, End: i + width}, decoded, Decoded, c.style, c.link)
		i += width
		start = i
	}
	p.verbatim(Span{Start: start, End: span.End}, c.style, c.link)
}

func decodeAt(s string) (int, string) {
	switch {
	case len(s) >= 2 && s[0] == '\\' && util.IsPunct(s[1]):
		return 2, s[1:2]
	case s[0] == '&':
		return entity(s)
	default:
		return 0, ""
	}
}

func entity(s string) (int, string) {
	end := strings.IndexByte(s, ';')
	if end < 2 || end > 33 {
		return 0, ""
	}
	name := s[1:end]
	if name[0] == '#' {
		return numericEntity(name[1:], end+1)
	}
	for i := range len(name) {
		if !isAlphanumeric(name[i]) {
			return 0, ""
		}
	}
	found, ok := util.LookUpHTML5EntityByName(name)
	if !ok {
		return 0, ""
	}
	return end + 1, string(found.Characters)
}

func numericEntity(digits string, width int) (int, string) {
	base, limit, body := numericBase(digits)
	if body == "" || len(body) > limit {
		return 0, ""
	}
	value, err := strconv.ParseUint(body, base, 32)
	if err != nil {
		return 0, ""
	}
	return width, numericRune(value)
}

func numericBase(digits string) (int, int, string) {
	if digits != "" && (digits[0] == 'x' || digits[0] == 'X') {
		return 16, 6, digits[1:]
	}
	return 10, 7, digits
}

func numericRune(value uint64) string {
	if value == 0 || value > uint64(utf8.MaxRune) {
		return string(utf8.RuneError)
	}
	r := rune(value)
	if !utf8.ValidRune(r) {
		return string(utf8.RuneError)
	}
	return string(r)
}

func isAlphanumeric(c byte) bool {
	return c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9'
}

func (b *builder) autoLink(p *projector, n *ast.AutoLink, c inlineContext) {
	label := string(n.Label(b.raw))
	destination := string(n.URL(b.raw))
	if n.AutoLinkType == ast.AutoLinkEmail && !strings.HasPrefix(strings.ToLower(destination), "mailto:") {
		destination = "mailto:" + destination
	}
	b.links = append(b.links, Link{Destination: destination})
	style := c.style | LinkText
	start := n.Pos()
	if start >= 0 && start < len(b.source) && !strings.HasPrefix(b.source[start:], label) {
		start++
	}
	if start < 0 || start > len(b.source) || !strings.HasPrefix(b.source[start:], label) {
		p.add(p.cursor(), label, Generated, style, len(b.links))
		return
	}
	p.verbatim(Span{Start: start, End: start + len(label)}, style, len(b.links))
}

func (b *builder) image(p *projector, n *ast.Image, c inlineContext) {
	at := n.Pos()
	marker := Span{Start: at, End: at}
	if at >= 0 && strings.HasPrefix(b.source[at:], "![") {
		marker.End = at + 2
	} else {
		marker = p.cursor()
	}
	p.add(marker, "image: ", Generated, c.style|ImageText, c.link)
}

func (b *builder) rawHTML(p *projector, n *ast.RawHTML, c inlineContext) {
	for i := range n.Segments.Len() {
		segment := n.Segments.At(i)
		p.verbatim(Span{Start: segment.Start, End: segment.Stop}, c.style|RawMarkup, c.link)
	}
}

func (b *builder) mathRuns(p *projector, n *mathNode, style Style, link int) {
	spans := make([]Span, 0, len(n.segments))
	for _, segment := range n.segments {
		spans = append(spans, Span{Start: segment.Start, End: segment.Stop})
	}
	for len(spans) > 0 && strings.TrimSpace(b.source[spans[0].Start:spans[0].End]) == "" {
		spans = spans[1:]
	}
	for len(spans) > 0 && strings.TrimSpace(b.source[spans[len(spans)-1].Start:spans[len(spans)-1].End]) == "" {
		spans = spans[:len(spans)-1]
	}
	if len(spans) == 0 {
		return
	}
	first, last := &spans[0], &spans[len(spans)-1]
	first.Start += len(b.source[first.Start:first.End]) - len(strings.TrimLeft(b.source[first.Start:first.End], mathSpace))
	last.End -= len(b.source[last.Start:last.End]) - len(strings.TrimRight(b.source[last.Start:last.End], mathSpace))
	var body strings.Builder
	for _, span := range spans {
		body.WriteString(b.source[span.Start:span.End])
	}
	p.add(n.delimited, body.String(), Formula, style, link)
}

func soleDisplayMath(n ast.Node) (*mathNode, bool) {
	var found *mathNode
	for child := n.FirstChild(); child != nil; child = child.NextSibling() {
		switch child := child.(type) {
		case *mathNode:
			if found != nil || !child.display {
				return nil, false
			}
			found = child
		case *ast.Text:
			if !child.Segment.IsEmpty() {
				return nil, false
			}
		default:
			return nil, false
		}
	}
	return found, found != nil
}

func (b *builder) tableProjection(n *east.Table) (Projection, []Span, []Alignment) {
	p := &projector{source: b.source}
	var rows []Span
	previous := -1
	for row := n.FirstChild(); row != nil; row = row.NextSibling() {
		if len(rows) > 0 {
			p.add(Span{Start: previous, End: cellSpan(row.FirstChild(), previous).Start}, "\n", Break, 0, 0)
			rows[len(rows)-1].End = p.text.Len()
		}
		rowStart := p.text.Len()
		for cell := row.FirstChild(); cell != nil; cell = cell.NextSibling() {
			span := cellSpan(cell, previous)
			if cell != row.FirstChild() {
				p.add(Span{Start: previous, End: span.Start}, "\t", separatorKind(previous, span.Start), 0, 0)
			}
			if !b.inline(p, cell, inlineContext{depth: 1}) {
				return b.tableFallback(n)
			}
			previous = span.End
		}
		rows = append(rows, Span{Start: rowStart, End: p.text.Len()})
	}
	return p.projection(), rows, tableAlignments(n.Alignments)
}

func separatorKind(start, end int) RunKind {
	if end > start {
		return Break
	}
	return Generated
}

func cellSpan(cell ast.Node, previous int) Span {
	if cell.Lines().Len() == 0 {
		return Span{Start: max(previous, 0), End: max(previous, 0)}
	}
	segment := cell.Lines().At(0)
	return Span{Start: segment.Start, End: segment.Stop}
}

func (b *builder) tableFallback(n *east.Table) (Projection, []Span, []Alignment) {
	start := b.nodeStart(n)
	b.diagnose(inlineNestingDiagnostic(Span{Start: start, End: start}))
	b.links = nil
	p := &projector{source: b.source}
	lineStart := b.lineStart(b.leafStart(n))
	end := lineStart
	for row := n.FirstChild(); row != nil; row = row.NextSibling() {
		for cell := row.FirstChild(); cell != nil; cell = cell.NextSibling() {
			end = max(end, cellSpan(cell, end).End)
		}
	}
	p.verbatim(Span{Start: lineStart, End: end}, 0, 0)
	projection := p.projection()
	rows, _ := rowsOf(KindCode, projection.Text)
	return projection, rows, tableAlignments(n.Alignments)
}

func tableAlignments(alignments []east.Alignment) []Alignment {
	out := make([]Alignment, len(alignments))
	for i, a := range alignments {
		switch a {
		case east.AlignLeft:
			out[i] = AlignLeft
		case east.AlignCenter:
			out[i] = AlignCenter
		case east.AlignRight:
			out[i] = AlignRight
		case east.AlignNone:
			out[i] = AlignNone
		}
	}
	return out
}
