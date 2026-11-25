package document

import (
	"bytes"

	"github.com/yuin/goldmark"
	"github.com/yuin/goldmark/ast"
	"github.com/yuin/goldmark/parser"
	"github.com/yuin/goldmark/text"
	"github.com/yuin/goldmark/util"
)

const mathSpace = " \t\r\n"

var (
	kindMath        = ast.NewNodeKind("Math")
	unclosedMathKey = parser.NewContextKey()
)

type mathNode struct {
	ast.BaseInline
	display   bool
	segments  []text.Segment
	delimited Span
}

func (n *mathNode) Kind() ast.NodeKind {
	return kindMath
}

func (n *mathNode) Dump(source []byte, level int) {
	ast.DumpHelper(n, source, level, nil, nil)
}

type mathExtension struct{}

func (mathExtension) Extend(m goldmark.Markdown) {
	m.Parser().AddOptions(parser.WithInlineParsers(util.Prioritized(mathParser{}, 150)))
}

type mathParser struct{}

type mathDelimiter struct {
	open    []byte
	close   []byte
	display bool
}

var mathDelimiters = []mathDelimiter{
	{open: []byte("$$"), close: []byte("$$"), display: true},
	{open: []byte("$"), close: []byte("$")},
	{open: []byte(`\(`), close: []byte(`\)`)},
	{open: []byte(`\[`), close: []byte(`\]`), display: true},
}

type unclosedMath struct {
	parent ast.Node
	kinds  uint8
}

func (mathParser) Trigger() []byte {
	return []byte{'$', '\\'}
}

func (mathParser) Parse(parent ast.Node, block text.Reader, pc parser.Context) ast.Node {
	line, opening := block.PeekLine()
	kind, ok := openingDelimiter(line)
	if !ok || unclosed(pc, parent)&(1<<kind) != 0 {
		return nil
	}
	delimiter := mathDelimiters[kind]
	block.Advance(len(delimiter.open))
	node := &mathNode{display: delimiter.display, delimited: Span{Start: opening.Start}}
	for {
		line, segment := block.PeekLine()
		if line == nil {
			markUnclosed(pc, parent, kind)
			return nil
		}
		if end := closingIndex(line, delimiter); end >= 0 {
			node.segments = append(node.segments, segment.WithStop(segment.Start+end))
			node.delimited.End = segment.Start + end + len(delimiter.close)
			block.Advance(end + len(delimiter.close))
			return nonBlankMath(node, block.Source())
		}
		node.segments = append(node.segments, segment)
		block.AdvanceLine()
	}
}

func unclosed(pc parser.Context, parent ast.Node) uint8 {
	if state, ok := pc.Get(unclosedMathKey).(unclosedMath); ok && state.parent == parent {
		return state.kinds
	}
	return 0
}

func markUnclosed(pc parser.Context, parent ast.Node, kind int) {
	pc.Set(unclosedMathKey, unclosedMath{parent: parent, kinds: unclosed(pc, parent) | 1<<kind})
}

func nonBlankMath(node *mathNode, source []byte) ast.Node {
	for _, segment := range node.segments {
		if len(bytes.TrimSpace(segment.Value(source))) > 0 {
			return node
		}
	}
	return nil
}

func openingDelimiter(line []byte) (int, bool) {
	for kind, d := range mathDelimiters {
		if !bytes.HasPrefix(line, d.open) {
			continue
		}
		if d.open[0] == '$' && !d.display && (len(line) < 2 || isMathSpace(line[1])) {
			return 0, false
		}
		return kind, true
	}
	return 0, false
}

func closingIndex(line []byte, d mathDelimiter) int {
	for i := 0; i < len(line); i++ {
		if bytes.HasPrefix(line[i:], d.close) && (d.close[0] != '$' || d.display || closesSingleDollar(line, i)) {
			return i
		}
		if line[i] == '\\' {
			i++
		}
	}
	return -1
}

func closesSingleDollar(line []byte, at int) bool {
	if at == 0 || isMathSpace(line[at-1]) {
		return false
	}
	return at+1 >= len(line) || line[at+1] < '0' || line[at+1] > '9'
}

func isMathSpace(c byte) bool {
	return c == ' ' || c == '\t' || c == '\n' || c == '\r'
}
