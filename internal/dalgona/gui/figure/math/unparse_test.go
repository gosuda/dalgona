package math

import (
	"strconv"
	"strings"
)

type texWriter struct {
	out           strings.Builder
	last          byte
	afterLetterCS bool
}

func unparse(formula *Formula) string {
	var w texWriter
	w.nodes(formula.Body)
	w.tag(formula.Tag)
	return w.out.String()
}

func (w *texWriter) raw(s string) {
	if s == "" {
		return
	}
	switch {
	case w.afterLetterCS && isASCIILetter(s[0]):
		w.out.WriteByte(' ')
	case numeric(w.last) && numeric(s[0]):
		w.out.WriteByte(' ')
	}
	w.out.WriteString(s)
	w.last = s[len(s)-1]
	w.afterLetterCS = false
}

func numeric(b byte) bool {
	return isDigit(b) || b == '.'
}

func (w *texWriter) command(name string) {
	w.raw("\\" + name)
	w.afterLetterCS = isASCIILetter(name[0])
}

func (w *texWriter) nodes(nodes []Node) {
	for _, node := range nodes {
		w.node(node)
	}
}

func (w *texWriter) braced(nodes []Node) {
	w.raw("{")
	w.nodes(nodes)
	w.raw("}")
}

func (w *texWriter) node(node Node) {
	switch n := node.(type) {
	case *Symbol:
		w.symbol(n.Command, n.Value)
	case *Number:
		w.raw(n.Digits)
	case *Group:
		w.braced(n.Body)
	case *Scripts:
		w.scripts(n)
	case *Fraction:
		w.command(n.Command)
		w.braced(n.Numerator)
		w.braced(n.Denominator)
	case *Radical:
		w.radical(n)
	default:
		w.structure(node)
	}
}

func (w *texWriter) structure(node Node) {
	switch n := node.(type) {
	case *Fence:
		w.fence(n)
	case *SizedDelimiter:
		w.command(n.Command)
		w.delimiter(n.Delimiter)
	case *Operator:
		w.operator(n)
	case *Accent:
		w.command(n.Command)
		w.braced(n.Body)
	case *Alphabet:
		w.command(n.Command)
		w.braced(n.Body)
	case *Text:
		w.command("text")
		w.text(n.Value)
	default:
		w.spacing(node)
	}
}

func (w *texWriter) spacing(node Node) {
	switch n := node.(type) {
	case *Space:
		w.space(n)
	case *StyleChange:
		w.command(n.Command)
	case *Environment:
		w.environment(n)
	default:
		panic("unparse: unknown node type")
	}
}

func (w *texWriter) symbol(command string, value rune) {
	if command != "" {
		w.command(command)
		return
	}
	w.raw(string(value))
}

func (w *texWriter) scripts(n *Scripts) {
	if n.Base != nil {
		w.node(n.Base)
	}
	w.raw(strings.Repeat("'", n.Primes))
	if n.Sub != nil {
		w.raw("_")
		w.braced(n.Sub.Body)
	}
	if n.Sup != nil {
		w.raw("^")
		w.braced(n.Sup.Body)
	}
}

func (w *texWriter) radical(n *Radical) {
	w.command("sqrt")
	if n.Index != nil {
		w.raw("[")
		w.nodes(n.Index.Body)
		w.raw("]")
	}
	w.braced(n.Body)
}

func (w *texWriter) fence(n *Fence) {
	w.command("left")
	w.delimiter(n.Left)
	for i, segment := range n.Segments {
		if i > 0 {
			w.command("middle")
			w.delimiter(n.Middles[i-1])
		}
		w.nodes(segment)
	}
	w.command("right")
	w.delimiter(n.Right)
}

func (w *texWriter) delimiter(d Delimiter) {
	if d.Value == 0 {
		w.raw(".")
		return
	}
	w.symbol(d.Command, d.Value)
}

func (w *texWriter) operator(n *Operator) {
	switch {
	case strings.HasPrefix(n.Command, "operatorname"):
		w.raw("\\" + n.Command)
		w.text(n.Name)
	case n.Command == "":
		w.raw(string(n.Value))
	default:
		w.command(n.Command)
	}
	switch n.Limits {
	case LimitsAbove:
		w.command("limits")
	case LimitsBeside:
		w.command("nolimits")
	case LimitsDefault:
	}
}

func (w *texWriter) text(value string) {
	var escaped strings.Builder
	for _, r := range value {
		switch r {
		case '{', '}', '$', '%', '&', '#', '_':
			escaped.WriteByte('\\')
			escaped.WriteRune(r)
		case '\u00a0':
			escaped.WriteByte('~')
		default:
			escaped.WriteRune(r)
		}
	}
	w.raw("{" + escaped.String() + "}")
}

func (w *texWriter) space(n *Space) {
	switch n.Command {
	case "~":
		w.raw("~")
	case "hspace":
		w.command("hspace")
		w.raw("{" + strconv.FormatFloat(float64(n.Amount), 'f', -1, 32) + n.Unit + "}")
	default:
		w.command(n.Command)
	}
}

func (w *texWriter) tag(tag *Tag) {
	if tag == nil {
		return
	}
	name := "tag"
	if tag.Star {
		name += "*"
	}
	w.raw("\\" + name)
	w.text(tag.Text)
}

func (w *texWriter) environment(n *Environment) {
	w.command("begin")
	w.raw("{" + n.Name + "}")
	if n.Columns != "" {
		w.raw("{" + n.Columns + "}")
	}
	if n.Pairs > 0 {
		w.raw("{" + strconv.Itoa(n.Pairs) + "}")
	}
	for i, row := range n.Rows {
		if i > 0 {
			w.command("\\")
		}
		for j, cell := range row.Cells {
			if j > 0 {
				w.raw("&")
			}
			w.nodes(cell)
		}
		w.tag(row.Tag)
	}
	w.command("end")
	w.raw("{" + n.Name + "}")
}
