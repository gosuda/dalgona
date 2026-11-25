package document

import (
	"bytes"

	"github.com/yuin/goldmark"
	"github.com/yuin/goldmark/ast"
	"github.com/yuin/goldmark/extension"
	"github.com/yuin/goldmark/parser"
	"github.com/yuin/goldmark/text"
	"github.com/yuin/goldmark/util"
)

func newMarkdownParser() parser.Parser {
	blocks := parser.DefaultBlockParsers()
	var scoped []util.PrioritizedValue
	for i, block := range blocks {
		switch block.Value {
		case parser.NewListParser():
			blocks[i].Value = depthLimitedList{BlockParser: parser.NewListParser()}
		case parser.NewBlockquoteParser():
			blocks[i].Value = depthLimitedContainer{BlockParser: parser.NewBlockquoteParser()}
		case parser.NewListItemParser():
			blocks[i].Value = depthLimitedContainer{BlockParser: parser.NewListItemParser()}
		default:
			if blockParser, ok := block.Value.(parser.BlockParser); ok {
				leaf := depthLimitedLeaf{BlockParser: blockParser}
				blocks[i].Value = leaf
				if !blockParser.CanInterruptParagraph() {
					scoped = append(scoped, util.Prioritized(virtualScopeLeaf{depthLimitedLeaf: leaf}, block.Priority-1))
				}
			}
		}
	}
	taskLists := scopedTaskList{InlineParser: extension.NewTaskCheckBoxParser()}
	p := parser.NewParser(
		parser.WithBlockParsers(append(blocks, scoped...)...),
		parser.WithInlineParsers(parser.DefaultInlineParsers()...),
		parser.WithInlineParsers(util.Prioritized(taskLists, 0)),
		parser.WithParagraphTransformers(parser.DefaultParagraphTransformers()...),
	)
	return goldmark.New(goldmark.WithParser(p),
		goldmark.WithExtensions(extension.Linkify, extension.Table, extension.Strikethrough, mathExtension{})).Parser()
}

type depthLimitedList struct{ parser.BlockParser }

func (p depthLimitedList) Open(parent ast.Node, reader text.Reader, pc parser.Context) (ast.Node, parser.State) {
	if containerDepth(parent) > MaxNesting {
		return nil, parser.NoChildren
	}
	return p.BlockParser.Open(parent, reader, pc)
}

type depthLimitedContainer struct{ parser.BlockParser }

const collapsedPrefix = "document:collapsed-container-prefix"

type containerPrefix struct {
	path           containerPath
	lazy           containerPath
	leaf           ast.Node
	empty          *emptyListLeaf
	changed        bool
	opened         bool
	firstTaskChild bool
}

type containerPath struct {
	steps  []int
	quotes []int
}

const quoteStep = 0

type emptyListLeaf struct {
	ast.TextBlock
	blank bool
}

func (n *emptyListLeaf) Pos() int { return n.BaseBlock.Pos() }

func collapsedContainer(node ast.Node) *containerPrefix {
	if node == nil {
		return nil
	}
	value, _ := node.AttributeString(collapsedPrefix)
	prefix, _ := value.(*containerPrefix)
	return prefix
}

func (p depthLimitedContainer) Open(parent ast.Node, reader text.Reader, pc parser.Context) (ast.Node, parser.State) {
	depth := containerDepth(parent)
	if depth > MaxNesting {
		return nil, parser.NoChildren
	}
	node, state := p.BlockParser.Open(parent, reader, pc)
	if state&parser.HasChildren == 0 || depth < MaxNesting {
		return node, state
	}
	prefix := &containerPrefix{firstTaskChild: node.Kind() == ast.KindListItem}
	prefix.open(node, reader, nil)
	node.SetAttributeString(collapsedPrefix, prefix)
	return node, state
}

func (p depthLimitedContainer) Continue(node ast.Node, reader text.Reader, pc parser.Context) parser.State {
	state := p.BlockParser.Continue(node, reader, pc)
	if state&parser.Continue == 0 {
		return state
	}
	if prefix := collapsedContainer(node); prefix != nil {
		prefix.continueAt(node, reader, pc.LastOpenedBlock().Node)
	}
	return state
}

func containerDepth(node ast.Node) int {
	depth := 0
	for ; node != nil; node = node.Parent() {
		switch node.Kind() {
		case ast.KindBlockquote, ast.KindListItem:
			depth++
		}
	}
	return depth
}

type depthLimitedLeaf struct{ parser.BlockParser }

func (p depthLimitedLeaf) Open(parent ast.Node, reader text.Reader, pc parser.Context) (ast.Node, parser.State) {
	prefix := collapsedContainer(parent)
	if prefix != nil && (prefix.changed || prefix.opened) {
		pc = withoutLastBlock{Context: pc}
	}
	node, state := p.BlockParser.Open(parent, reader, pc)
	if node != nil && prefix != nil {
		prefix.addLeaf(parent, node, pc)
	}
	return node, state
}

type scopedTaskList struct{ parser.InlineParser }

var scopedTaskLines = parser.NewContextKey()

var scopedTaskParagraph = func() *ast.Paragraph {
	item := ast.NewListItem(0)
	paragraph := ast.NewParagraph()
	item.AppendChild(item, paragraph)
	return paragraph
}()

func (p scopedTaskList) Parse(parent ast.Node, reader text.Reader, pc parser.Context) ast.Node {
	if collapsedContainer(parent.Parent()) == nil {
		return p.InlineParser.Parse(parent, reader, pc)
	}
	lines, _ := pc.Get(scopedTaskLines).(map[*text.Segments]struct{})
	if _, eligible := lines[parent.Lines()]; !eligible || parent.HasChildren() {
		return nil
	}
	return p.InlineParser.Parse(scopedTaskParagraph, reader, pc)
}

func (p *containerPrefix) addLeaf(parent, child ast.Node, pc parser.Context) {
	p.fillEmpty(parent)
	if !p.firstTaskChild {
		return
	}
	p.firstTaskChild = false
	if child.IsRaw() {
		return
	}
	lines, _ := pc.Get(scopedTaskLines).(map[*text.Segments]struct{})
	if lines == nil {
		lines = make(map[*text.Segments]struct{})
		pc.Set(scopedTaskLines, lines)
	}
	lines[child.Lines()] = struct{}{}
}

func (p depthLimitedLeaf) Continue(node ast.Node, reader text.Reader, pc parser.Context) parser.State {
	if prefix := collapsedContainer(node.Parent()); prefix != nil && prefix.changed && !ast.IsParagraph(node) {
		return parser.Close
	}
	return p.BlockParser.Continue(node, reader, pc)
}

type virtualScopeLeaf struct{ depthLimitedLeaf }

func (virtualScopeLeaf) CanInterruptParagraph() bool { return true }

func (p virtualScopeLeaf) Open(parent ast.Node, reader text.Reader, pc parser.Context) (ast.Node, parser.State) {
	if prefix := collapsedContainer(parent); prefix != nil && prefix.opened {
		return p.depthLimitedLeaf.Open(parent, reader, pc)
	}
	return nil, parser.NoChildren
}

type withoutLastBlock struct{ parser.Context }

func (withoutLastBlock) LastOpenedBlock() parser.Block { return parser.Block{} }

func (p *containerPrefix) continueAt(node ast.Node, reader text.Reader, leaf ast.Node) {
	if ast.IsParagraph(leaf) && p.leaf == leaf {
		p.path = p.lazy
	}
	p.lazy, p.leaf, p.opened = containerPath{}, nil, false
	matched, quotes := p.match(reader)
	p.changed = matched < len(p.path.steps)
	if p.changed {
		p.lazy, p.leaf = p.path, leaf
		p.path = containerPath{steps: p.path.steps[:matched:matched], quotes: p.path.quotes[:quotes:quotes]}
		p.empty = nil
		p.firstTaskChild = false
	} else if staysRaw(node, leaf) {
		return
	}
	if p.changed {
		leaf = nil
	}
	p.open(node, reader, leaf)
	p.changed = p.changed || p.opened
}

func staysRaw(container, leaf ast.Node) bool {
	if leaf == nil || leaf.Parent() != container {
		return false
	}
	return leaf.Kind() == ast.KindFencedCodeBlock || leaf.Kind() == ast.KindHTMLBlock
}

func (p *containerPrefix) fillEmpty(parent ast.Node) {
	if p.empty != nil {
		parent.RemoveChild(parent, p.empty)
		p.empty = nil
	}
}

func (p *containerPrefix) open(node ast.Node, reader text.Reader, previous ast.Node) {
	line := newContainerLine(reader)
	rule := thematicSuffix(line.bytes)
	for {
		next := line
		marker, ok := next.open(rule)
		if !ok || !p.opened && ast.IsParagraph(previous) && !marker.interrupts {
			break
		}
		line = next
		p.opened = true
		p.fillEmpty(node)
		p.firstTaskChild = marker.indent != quoteStep
		if marker.indent == quoteStep {
			p.path.quotes = append(p.path.quotes, len(p.path.steps))
		}
		p.path.steps = append(p.path.steps, marker.indent)
		if marker.empty {
			p.empty = &emptyListLeaf{}
			_, pos := reader.Position()
			p.empty.SetPos(pos.Start + line.at - pos.Padding)
			node.AppendChild(node, p.empty)
			break
		}
	}
	line.advance(reader)
}

func (p *containerPrefix) match(reader text.Reader) (int, int) {
	stop := len(p.path.steps)
	if p.empty != nil && p.empty.blank {
		stop--
	}
	matched, quotes, blank := p.path.match(reader, stop)
	if blank && p.empty != nil {
		p.empty.blank = true
	}
	return matched, quotes
}

func (p containerPath) match(reader text.Reader, stop int) (int, int, bool) {
	if len(p.steps) == 0 {
		return 0, 0, false
	}
	line := newContainerLine(reader)
	blank := trailingBlankStart(line.bytes)
	matched, quotes := 0, 0
	for _, indent := range p.steps {
		if indent != quoteStep && line.at >= blank {
			line.advance(reader)
			reader.AdvanceToEOL()
			return p.nextQuote(quotes), quotes, true
		}
		if matched == stop || !line.continues(indent) {
			break
		}
		matched++
		if indent == quoteStep {
			quotes++
		}
	}
	line.advance(reader)
	return matched, quotes, false
}

func (p containerPath) nextQuote(quotes int) int {
	if quotes < len(p.quotes) {
		return p.quotes[quotes]
	}
	return len(p.steps)
}

func trailingBlankStart(line []byte) int {
	end := len(line)
	for end > 0 && util.IsSpace(line[end-1]) {
		end--
	}
	return end
}

type containerLine struct {
	bytes   []byte
	at      int
	column  int
	padding int
}

func newContainerLine(reader text.Reader) containerLine {
	line, _ := reader.PeekLine()
	return containerLine{bytes: line, column: reader.LineOffset()}
}

func (l *containerLine) advance(reader text.Reader) {
	if l.at > 0 {
		reader.AdvanceAndSetPadding(l.at, l.padding)
	}
}

type containerMarker struct {
	indent     int
	interrupts bool
	empty      bool
}

func (l *containerLine) open(rule int) (containerMarker, bool) {
	if l.quote() {
		return containerMarker{indent: quoteStep, interrupts: true}, true
	}
	width, spaces := l.indent()
	start := l.at + spaces
	if width > 3 || start >= len(l.bytes) || start >= rule || bytes.IndexByte(l.bytes[l.at:start], '\t') >= 0 {
		return containerMarker{}, false
	}
	end := listMarkerEnd(l.bytes, start)
	if end == start {
		return containerMarker{}, false
	}
	rest := l.bytes[end:]
	if util.IsBlank(rest) {
		l.take(end, 0)
		return containerMarker{indent: width + end - start + 1, empty: true}, true
	}
	return containerMarker{
		indent:     l.openList(rest, end, width+end-start),
		interrupts: !util.IsNumeric(l.bytes[start]) || bytes.Equal(bytes.TrimLeft(l.bytes[start:end-1], "0"), []byte("1")),
	}, true
}

func (l *containerLine) indent() (int, int) {
	width, bytes := util.IndentWidth(l.bytes[l.at:], l.column+l.padding)
	return width + l.padding, bytes
}

func (l *containerLine) quote() bool {
	width, spaces := l.indent()
	mark := l.at + spaces
	if width > 3 || mark == len(l.bytes) || l.bytes[mark] != '>' {
		return false
	}
	end := mark + 1
	switch {
	case end < len(l.bytes) && l.bytes[end] == '\t':
		l.take(end+1, util.TabWidth(l.column+width+1)-1)
	case end < len(l.bytes) && l.bytes[end] == ' ':
		l.take(end+1, 0)
	default:
		l.take(end, 0)
	}
	return true
}

func (l *containerLine) openList(rest []byte, end, markerWidth int) int {
	indent, _ := util.IndentWidth(rest, markerWidth)
	if indent > 4 {
		indent = 1
	}
	spaces, padding := util.IndentPosition(rest, markerWidth, indent)
	l.take(end+spaces, padding)
	return markerWidth + indent
}

func (l *containerLine) continues(indent int) bool {
	if indent == quoteStep {
		return l.quote()
	}
	spaces, padding := util.IndentPosition(l.bytes[l.at:], l.column+l.padding, max(0, indent-l.padding))
	if spaces < 0 {
		return false
	}
	l.take(l.at+spaces, padding+max(0, l.padding-indent))
	return true
}

func (l *containerLine) take(end, padding int) {
	l.column += l.padding
	for _, b := range l.bytes[l.at:end] {
		if b == '\t' {
			l.column += util.TabWidth(l.column)
		} else {
			l.column++
		}
	}
	l.column -= padding
	l.at, l.padding = end, padding
}

func listMarkerEnd(line []byte, start int) int {
	end := listMarkerTokenEnd(line, start)
	if end == len(line) {
		return end
	}
	switch line[end] {
	case ' ', '\t', '\n':
		return end
	default:
		return start
	}
}

func listMarkerTokenEnd(line []byte, start int) int {
	switch line[start] {
	case '-', '*', '+':
		return start + 1
	}
	end := start
	for end < len(line) && end-start < 9 && util.IsNumeric(line[end]) {
		end++
	}
	if end == start || end == len(line) {
		return start
	}
	switch line[end] {
	case '.', ')':
		return end + 1
	}
	return start
}

func thematicSuffix(line []byte) int {
	marker := byte(0)
	start, count := len(line), 0
	for i := len(line) - 1; i >= 0; i-- {
		b := line[i]
		if util.IsSpace(b) {
			continue
		}
		if marker == 0 {
			marker = b
		}
		if b != marker || b != '-' && b != '*' {
			break
		}
		start, count = i, count+1
	}
	if count < 3 {
		return len(line)
	}
	return start
}
