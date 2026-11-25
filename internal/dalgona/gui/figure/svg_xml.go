package figure

import (
	"bytes"
	"encoding/xml"
	"errors"
	"io"
	"sort"
	"strings"
)

const (
	svgNamespace   = "http://www.w3.org/2000/svg"
	xlinkNamespace = "http://www.w3.org/1999/xlink"
	xmlNamespace   = "http://www.w3.org/XML/1998/namespace"
)

type svgAttr struct {
	space string
	name  string
	value string
}

type svgNode struct {
	name     string
	attrs    []svgAttr
	children []*svgNode
	parent   *svgNode
	text     string
	isText   bool
	span     Span
	classes  []string
}

func (n *svgNode) attr(name string) (string, bool) {
	for _, a := range n.attrs {
		if a.name == name && (a.space == "" || a.space == svgNamespace) {
			return a.value, true
		}
	}
	return "", false
}

func (n *svgNode) href() (string, bool) {
	for _, a := range n.attrs {
		if a.name == "href" && (a.space == "" || a.space == xlinkNamespace) {
			return a.value, true
		}
	}
	return "", false
}

func (n *svgNode) preservesSpace() bool {
	for _, a := range n.attrs {
		if a.name == "space" && (a.space == "xml" || a.space == xmlNamespace) {
			return a.value == "preserve"
		}
	}
	return false
}

func (n *svgNode) textContent() string {
	var b strings.Builder
	for _, c := range n.children {
		if c.isText {
			b.WriteString(c.text)
		} else {
			b.WriteString(c.textContent())
		}
	}
	return b.String()
}

type lineIndex []int

func newLineIndex(src []byte) lineIndex {
	starts := lineIndex{0}
	for i, c := range src {
		if c == '\n' {
			starts = append(starts, i+1)
		}
	}
	return starts
}

func (l lineIndex) span(offset int64) Span {
	i := sort.Search(len(l), func(i int) bool { return int64(l[i]) > offset }) - 1
	i = max(i, 0)
	line, column := i+1, int(offset)-l[i]+1
	return Span{Line: line, Column: column, EndLine: line, EndColumn: column}
}

func scanDeclarations(src []byte, lines lineIndex) error {
	for i := 0; i < len(src); {
		j := bytes.IndexByte(src[i:], '<')
		if j < 0 {
			return nil
		}
		i += j
		end, err := skipMarkup(src, i, lines)
		if err != nil {
			return err
		}
		i = end
	}
	return nil
}

var markupSkips = []struct{ open, close string }{
	{"<!--", "-->"},
	{"<![CDATA[", "]]>"},
	{"<?", "?>"},
}

func skipMarkup(src []byte, i int, lines lineIndex) (int, error) {
	for _, m := range markupSkips {
		if bytes.HasPrefix(src[i:], []byte(m.open)) {
			k := bytes.Index(src[i+len(m.open):], []byte(m.close))
			if k < 0 {
				return 0, failAt(FailureInvalid, lines.span(int64(i)), "markup is not terminated; close it with %s", m.close)
			}
			return i + len(m.open) + k + len(m.close), nil
		}
	}
	if bytes.HasPrefix(src[i:], []byte("<!")) {
		return 0, failAt(FailureForbidden, lines.span(int64(i)), "document type and entity declarations are refused; remove the <! declaration")
	}
	return i + 1, nil
}

type treeBuilder struct {
	dec      *xml.Decoder
	lines    lineIndex
	root     *svgNode
	stack    []*svgNode
	elements int
}

func parseTree(src []byte) (*svgNode, error) {
	lines := newLineIndex(src)
	if err := scanDeclarations(src, lines); err != nil {
		return nil, err
	}
	dec := xml.NewDecoder(bytes.NewReader(src))
	dec.Strict = true
	b := &treeBuilder{dec: dec, lines: lines}
	for {
		offset := dec.InputOffset()
		tok, err := dec.Token()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return nil, failAt(FailureInvalid, lines.span(dec.InputOffset()), "SVG is not well-formed XML: %s", xmlMessage(err))
		}
		if err := b.token(tok, lines.span(offset)); err != nil {
			return nil, err
		}
	}
	if b.root == nil || len(b.stack) != 0 {
		return nil, fail(FailureInvalid, "SVG has no complete root element; add an <svg> element")
	}
	return b.root, nil
}

func xmlMessage(err error) string {
	var syntax *xml.SyntaxError
	if errors.As(err, &syntax) {
		return syntax.Msg
	}
	return err.Error()
}

func (b *treeBuilder) token(tok xml.Token, span Span) error {
	switch t := tok.(type) {
	case xml.StartElement:
		return b.start(t, span)
	case xml.EndElement:
		if len(b.stack) == 0 {
			return failAt(FailureInvalid, span, "closing tag </%s> has no opening tag", t.Name.Local)
		}
		b.stack = b.stack[:len(b.stack)-1]
	case xml.CharData:
		b.characters(string(t), span)
	case xml.Directive:
		return failAt(FailureForbidden, span, "document type and entity declarations are refused; remove the <! declaration")
	case xml.Comment, xml.ProcInst:
	}
	return nil
}

func (b *treeBuilder) start(t xml.StartElement, span Span) error {
	if len(b.stack) == 0 && b.root != nil {
		return failAt(FailureInvalid, span, "SVG has more than one root element")
	}
	if len(b.stack) >= SVGDepthLimit {
		return failAt(FailureLimit, span, "SVG nests deeper than %d levels; flatten its groups", SVGDepthLimit)
	}
	b.elements++
	if b.elements > SVGElementLimit {
		return failAt(FailureLimit, span, "SVG has more than %d elements; simplify the figure", SVGElementLimit)
	}
	if t.Name.Space != "" && t.Name.Space != svgNamespace {
		return failAt(FailureInvalid, span, "element <%s> in namespace %s is outside the SVG profile; remove it", t.Name.Local, t.Name.Space)
	}
	node := elementNode(t, span)
	if len(b.stack) == 0 {
		b.root = node
	} else {
		parent := b.stack[len(b.stack)-1]
		node.parent = parent
		parent.children = append(parent.children, node)
	}
	b.stack = append(b.stack, node)
	return nil
}

func elementNode(t xml.StartElement, span Span) *svgNode {
	node := &svgNode{name: t.Name.Local, span: span}
	for _, a := range t.Attr {
		node.attrs = append(node.attrs, svgAttr{space: attributeSpace(a.Name.Space), name: a.Name.Local, value: a.Value})
	}
	if class, ok := node.attr("class"); ok {
		node.classes = strings.Fields(class)
	}
	return node
}

func attributeSpace(space string) string {
	switch space {
	case "xlink":
		return xlinkNamespace
	case "xml":
		return xmlNamespace
	}
	return space
}

func (b *treeBuilder) characters(data string, span Span) {
	if len(b.stack) == 0 {
		return
	}
	parent := b.stack[len(b.stack)-1]
	if n := len(parent.children); n > 0 && parent.children[n-1].isText {
		parent.children[n-1].text += data
		return
	}
	parent.children = append(parent.children, &svgNode{isText: true, text: data, span: span, parent: parent})
}
