package document

import (
	"strings"

	"github.com/yuin/goldmark/ast"
	east "github.com/yuin/goldmark/extension/ast"
	"github.com/yuin/goldmark/parser"
	"github.com/yuin/goldmark/text"
	"github.com/yuin/goldmark/util"
)

var markdownParser = newMarkdownParser()

var figureLanguages = map[string]bool{"mermaid": true, "d2": true, "nomnoml": true, "svg": true}

type definition struct {
	label       string
	destination string
	title       string
	start       int
}

type region struct {
	start       int
	first       int
	definition  bool
	labels      []string
	diagnostics []Diagnostic
}

type parsed struct {
	blocks      []Block
	regions     []region
	definitions []definition
}

type builder struct {
	source     string
	raw        []byte
	out        parsed
	containers int
	deep       bool
	links      []Link
	task       Task
}

func parseMarkdown(source string, from, to int, definitions []definition) parsed {
	context := parser.NewContext()
	for _, d := range definitions {
		context.AddReference(parser.NewReference([]byte(d.label), []byte(d.destination), []byte(d.title)))
	}
	region := source[from:to]
	raw := []byte(region)
	root := markdownParser.Parse(text.NewReader(raw), parser.WithContext(context))
	b := &builder{source: region, raw: raw}
	b.document(root)
	markFirstLeaves(b.out.blocks)
	b.out.shift(from)
	for i := range b.out.blocks {
		b.out.blocks[i].Fingerprint = projectionFingerprint(b.out.blocks[i].Projection)
	}
	for i := range b.out.regions {
		end := to
		if i+1 < len(b.out.regions) {
			end = b.out.regions[i+1].start
		}
		b.out.regions[i].labels = bracketLabels(source[b.out.regions[i].start:end])
	}
	return b.out
}

func (b *builder) document(root ast.Node) {
	for n := root.FirstChild(); n != nil; n = n.NextSibling() {
		_, isDefinition := n.(*ast.LinkReferenceDefinition)
		b.out.regions = append(b.out.regions, region{
			start:      b.lineStart(b.leafStart(n)),
			first:      len(b.out.blocks),
			definition: isDefinition,
		})
		b.deep = false
		b.node(n, nil)
	}
}

func (b *builder) node(n ast.Node, containers []Container) {
	switch n := n.(type) {
	case *ast.Blockquote:
		b.children(n, b.nest(containers, Container{Kind: ContainerQuotation}, n))
	case *ast.List:
		b.list(n, containers)
	case *ast.LinkReferenceDefinition:
		b.out.definitions = append(b.out.definitions, definition{
			label:       util.ToLinkReference(n.Label),
			destination: string(n.Destination),
			title:       string(n.Title),
			start:       b.nodeStart(n),
		})
	default:
		b.leaf(n, containers)
	}
}

func (b *builder) children(n ast.Node, containers []Container) {
	for c := n.FirstChild(); c != nil; c = c.NextSibling() {
		b.node(c, containers)
	}
}

func (b *builder) nest(containers []Container, c Container, at ast.Node) []Container {
	if len(containers) >= MaxNesting {
		if !b.deep {
			start := b.nodeStart(at)
			b.diagnose(nestingDiagnostic(Span{Start: start, End: start}))
		}
		b.deep = true
		return containers
	}
	b.containers++
	c.id = b.containers
	nested := make([]Container, len(containers), len(containers)+1)
	copy(nested, containers)
	return append(nested, c)
}

func (b *builder) list(n *ast.List, containers []Container) {
	number := n.Start
	for item := n.FirstChild(); item != nil; item = item.NextSibling() {
		nested := b.nest(containers, Container{
			Kind:    ContainerListItem,
			Ordered: n.IsOrdered(),
			Tight:   n.IsTight,
			Marker:  n.Marker,
			Start:   n.Start,
			Number:  number,
		}, item)
		if item.ChildCount() == 0 {
			b.emit(Block{Kind: KindParagraph, Containers: nested}, b.nodeStart(item))
		}
		b.children(item, nested)
		number++
	}
}

func (b *builder) leaf(n ast.Node, containers []Container) {
	block := Block{Containers: containers}
	b.links, b.task = nil, TaskNone
	switch n := n.(type) {
	case *ast.Heading:
		block.Kind, block.Level = KindHeading, n.Level
		block.Projection = b.inlineProjection(n)
	case *ast.ThematicBreak:
		block.Kind = KindRule
	case *ast.CodeBlock:
		block.Kind = KindCode
		block.Projection = b.lineProjection(n.Lines(), 0)
	case *ast.FencedCodeBlock:
		b.fenced(n, &block)
	case *ast.HTMLBlock:
		block.Kind = KindHTML
		block.Projection = b.htmlProjection(n)
	case *east.Table:
		block.Kind = KindTable
		block.Projection, block.Rows, block.Alignments = b.tableProjection(n)
	default:
		b.paragraph(n, &block)
	}
	block.Links, block.Task = b.links, b.task
	b.emit(block, b.leafStart(n))
}

func (b *builder) emit(block Block, start int) {
	if block.Body == (Span{}) {
		block.Body = Span{Start: 0, End: len(block.Projection.Text)}
	}
	lineStart := b.lineStart(start)
	if block.Rows == nil {
		var wrapped bool
		block.Rows, wrapped = rowsOf(block.Kind, block.Projection.Text)
		if wrapped {
			b.diagnose(rowDiagnostic(Span{Start: lineStart, End: lineStart}))
		}
	}
	block.Span = Span{Start: lineStart, End: lineStart}
	block.Fingerprint = projectionFingerprint(block.Projection)
	b.out.blocks = append(b.out.blocks, block)
}

func markFirstLeaves(blocks []Block) {
	var previous []Container
	for i := range blocks {
		current := blocks[i].Containers
		if len(current) == 0 {
			previous = current
			continue
		}
		marked := make([]Container, len(current))
		for k, c := range current {
			c.First = k >= len(previous) || previous[k].id != c.id
			marked[k] = c
		}
		blocks[i].Containers = marked
		previous = current
	}
}

func (b *builder) paragraph(n ast.Node, block *Block) {
	block.Kind = KindParagraph
	if math, ok := soleDisplayMath(n); ok {
		block.Kind = KindMath
		if len(math.segments) > 0 {
			block.Math = Span{Start: math.segments[0].Start, End: math.segments[len(math.segments)-1].Stop}
		}
		p := &projector{source: b.source}
		b.mathRuns(p, math, 0, 0)
		block.Projection = p.projection()
		return
	}
	block.Projection = b.inlineProjection(n)
}

func (b *builder) fenced(n *ast.FencedCodeBlock, block *Block) {
	block.Language = string(n.Language(b.raw))
	block.Kind = KindCode
	if !figureLanguages[strings.ToLower(block.Language)] {
		block.Projection = b.lineProjection(n.Lines(), 0)
		return
	}
	block.Kind = KindFigure
	p := &projector{source: b.source}
	info := Span{Start: n.Info.Segment.Start, End: n.Info.Segment.Stop}
	p.verbatim(info, 0, 0)
	if n.Lines().Len() > 0 {
		p.add(Span{Start: info.End, End: n.Lines().At(0).Start}, "\n", Break, 0, 0)
	}
	bodyStart := p.text.Len()
	b.lines(p, n.Lines(), 0)
	block.Projection = p.projection()
	block.Body = Span{Start: bodyStart, End: len(block.Projection.Text)}
}

func (b *builder) lineProjection(lines *text.Segments, style Style) Projection {
	p := &projector{source: b.source}
	b.lines(p, lines, style)
	return p.projection()
}

func (b *builder) lines(p *projector, lines *text.Segments, style Style) {
	for i := range lines.Len() {
		segment := lines.At(i)
		b.padding(p, segment, style)
		p.verbatim(Span{Start: segment.Start, End: segment.Stop}, style, 0)
	}
}

func (b *builder) padding(p *projector, segment text.Segment, style Style) {
	if segment.Padding == 0 {
		return
	}
	at := Span{Start: segment.Start, End: segment.Start}
	if segment.Start > 0 && b.source[segment.Start-1] == '\t' {
		at.Start--
	}
	p.add(at, strings.Repeat(" ", segment.Padding), Generated, style, 0)
}

func (b *builder) htmlProjection(n *ast.HTMLBlock) Projection {
	p := &projector{source: b.source}
	b.lines(p, n.Lines(), RawMarkup)
	if n.HasClosure() {
		p.verbatim(Span{Start: n.ClosureLine.Start, End: n.ClosureLine.Stop}, RawMarkup, 0)
	}
	return p.projection()
}

func (b *builder) nodeStart(n ast.Node) int {
	if pos := n.Pos(); pos >= 0 {
		return pos
	}
	if n.Type() == ast.TypeBlock && n.Lines().Len() > 0 {
		return n.Lines().At(0).Start
	}
	if n.FirstChild() != nil {
		return b.nodeStart(n.FirstChild())
	}
	return 0
}

func (b *builder) leafStart(n ast.Node) int {
	if table, ok := n.(*east.Table); ok && table.FirstChild() != nil {
		return b.nodeStart(table.FirstChild())
	}
	return b.nodeStart(n)
}

func (b *builder) lineStart(offset int) int {
	return strings.LastIndexByte(b.source[:offset], '\n') + 1
}

func (out *parsed) shift(base int) {
	for i := range out.blocks {
		block := &out.blocks[i]
		block.Span.Start += base
		block.Span.End += base
		if block.Math.Len() > 0 {
			block.Math.Start += base
			block.Math.End += base
		}
		for j := range block.Projection.Runs {
			block.Projection.Runs[j].Source.Start += base
			block.Projection.Runs[j].Source.End += base
		}
	}
	for i := range out.regions {
		out.regions[i].start += base
	}
	for i := range out.definitions {
		out.definitions[i].start += base
	}
	for i := range out.regions {
		for j := range out.regions[i].diagnostics {
			out.regions[i].diagnostics[j].Span.Start += base
			out.regions[i].diagnostics[j].Span.End += base
		}
	}
}

func (b *builder) diagnose(d Diagnostic) {
	r := &b.out.regions[len(b.out.regions)-1]
	r.diagnostics = append(r.diagnostics, d)
}

func bracketLabels(source string) []string {
	var labels []string
	seen := map[string]bool{}
	for rest := source; ; {
		open := strings.IndexByte(rest, '[')
		if open < 0 {
			return labels
		}
		rest = rest[open+1:]
		end := strings.IndexByte(rest, ']')
		if end < 0 {
			return labels
		}
		if end > 999 {
			continue
		}
		label := util.ToLinkReference([]byte(rest[:end]))
		if !seen[label] {
			seen[label] = true
			labels = append(labels, label)
		}
	}
}
